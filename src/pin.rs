// SPDX-License-Identifier: GPL-3.0-only

use crate::app::SHELF_SIZE;
use cosmic::cctk::sctk::shell::wlr_layer::Anchor;
use cosmic::iced::platform_specific::runtime::wayland::layer_surface::IcedMargin;
use std::{
    collections::BTreeSet,
    env,
    fs::{self, File, OpenOptions, TryLockError},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, Read, Seek, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
    time::Instant,
};

const GAP: u32 = 12;

pub struct Motion {
    from: IcedMargin,
    to: IcedMargin,
    started: Instant,
}

impl Motion {
    pub fn new(from: IcedMargin, to: IcedMargin) -> Self {
        Self {
            from,
            to,
            started: Instant::now(),
        }
    }

    pub fn frame(&self) -> (IcedMargin, bool) {
        let progress = (self.started.elapsed().as_secs_f32() / 0.22).min(1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        let interpolate =
            |from: i32, to: i32| (from as f32 + (to as f32 - from as f32) * eased).round() as i32;
        (
            IcedMargin {
                top: interpolate(self.from.top, self.to.top),
                right: interpolate(self.from.right, self.to.right),
                bottom: interpolate(self.from.bottom, self.to.bottom),
                left: interpolate(self.from.left, self.to.left),
            },
            progress >= 1.0,
        )
    }
}

#[derive(Debug, Clone)]
pub struct Area {
    pub width: u32,
    pub height: u32,
    pub output: u32,
}

pub struct Startup {
    pub area: Area,
    pub anchor: Anchor,
    pub near: (u32, u32),
}

impl Startup {
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(value) = env::var_os("ORBIT_START_PINNED") else {
            return Ok(None);
        };
        let values = value
            .to_str()
            .ok_or_else(|| io::Error::other("Invalid pin startup"))?
            .split(',')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(io::Error::other)?;
        let [width, height, output, bits, x, y] = values.as_slice() else {
            return Err(io::Error::other("Invalid pin startup"));
        };
        let anchor = Anchor::from_bits(*bits)
            .filter(|anchor| {
                anchor.bits().count_ones() == 2
                    && anchor.intersects(Anchor::TOP | Anchor::BOTTOM)
                    && anchor.intersects(Anchor::LEFT | Anchor::RIGHT)
            })
            .ok_or_else(|| io::Error::other("Invalid pin corner"))?;
        if *output == 0
            || *width == 0
            || *height == 0
            || *width > i32::MAX as u32
            || *height > i32::MAX as u32
            || *x >= *width
            || *y >= *height
        {
            return Err(io::Error::other("Invalid pin area"));
        }
        Ok(Some(Self {
            area: Area {
                width: *width,
                height: *height,
                output: *output,
            },
            anchor,
            near: (*x, *y),
        }))
    }

    pub fn encode(&self) -> String {
        format!(
            "{},{},{},{},{},{}",
            self.area.width,
            self.area.height,
            self.area.output,
            self.anchor.bits(),
            self.near.0,
            self.near.1
        )
    }
}

pub struct Placement {
    directory: PathBuf,
    area: Area,
    output: u64,
    lease: File,
    pub margin: IcedMargin,
    pub width: u32,
    pub height: u32,
}

impl Placement {
    pub fn new(
        area: Area,
        anchor: Anchor,
        near: Option<(u32, u32)>,
        width: u32,
        height: u32,
    ) -> io::Result<Self> {
        if !(1..=SHELF_SIZE).contains(&width) || !(1..=SHELF_SIZE).contains(&height) {
            return Err(io::Error::other("Invalid shelf size"));
        }
        let runtime = env::var_os("XDG_RUNTIME_DIR")
            .ok_or_else(|| io::Error::other("Missing runtime directory"))?;
        let directory = PathBuf::from(runtime).join("io.github.motixo.Orbit/pins");
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let mut hasher = DefaultHasher::new();
        env::var_os("WAYLAND_DISPLAY").hash(&mut hasher);
        area.output.hash(&mut hasher);
        let registry = open(directory.join("layout.lock"))?;
        registry.lock()?;
        let lease = open(directory.join(format!("{}.pin", std::process::id())))?;
        lease.try_lock().map_err(io::Error::from)?;
        let mut placement = Self {
            directory,
            area,
            output: hasher.finish(),
            lease,
            margin: IcedMargin::default(),
            width,
            height,
        };
        placement.place(anchor, near)?;
        Ok(placement)
    }

    pub fn startup(&self, anchor: Anchor) -> Startup {
        Startup {
            area: self.area.clone(),
            anchor,
            near: (self.margin.left as u32, self.margin.top as u32),
        }
    }

    pub fn corner_at(&self, position: cosmic::iced::Point) -> Anchor {
        let horizontal = if self.margin.left as f32 + position.x < self.area.width as f32 / 2.0 {
            Anchor::LEFT
        } else {
            Anchor::RIGHT
        };
        let vertical = if self.margin.top as f32 + position.y < self.area.height as f32 / 2.0 {
            Anchor::TOP
        } else {
            Anchor::BOTTOM
        };
        horizontal | vertical
    }

    pub fn drag_position(&self, delta: cosmic::iced::Vector) -> (i32, i32) {
        (
            self.margin.left.saturating_add(delta.x.round() as i32),
            self.margin.top.saturating_add(delta.y.round() as i32),
        )
    }

    pub fn move_to(&mut self, anchor: Anchor) -> io::Result<IcedMargin> {
        let registry = open(self.directory.join("layout.lock"))?;
        registry.lock()?;
        self.place(anchor, None)
    }

    pub fn resize(&mut self, width: u32, height: u32, anchor: Anchor) -> io::Result<IcedMargin> {
        if !(1..=SHELF_SIZE).contains(&width) || !(1..=SHELF_SIZE).contains(&height) {
            return Err(io::Error::other("Invalid shelf size"));
        }
        let registry = open(self.directory.join("layout.lock"))?;
        registry.lock()?;
        let previous_size = (self.width, self.height);
        self.width = width;
        self.height = height;
        match self.place(anchor, None) {
            Ok(margin) => Ok(margin),
            Err(error) => {
                (self.width, self.height) = previous_size;
                Err(error)
            }
        }
    }

    pub fn refresh(&mut self, anchor: Anchor) -> io::Result<Option<IcedMargin>> {
        let registry = open(self.directory.join("layout.lock"))?;
        match registry.try_lock() {
            Ok(()) => {
                let previous = self.margin;
                let margin = self.place(anchor, None)?;
                Ok((margin.left != previous.left || margin.top != previous.top).then_some(margin))
            }
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    fn place(&mut self, anchor: Anchor, near: Option<(u32, u32)>) -> io::Result<IcedMargin> {
        let mut occupied = Vec::new();
        let own_name = format!("{}.pin", std::process::id());
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if entry.file_name() == own_name.as_str()
                || entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "pin")
            {
                continue;
            }
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(entry.path())?;
            match file.try_lock() {
                // A process exit releases its lease, including after a crash.
                Ok(()) => fs::remove_file(entry.path())?,
                Err(TryLockError::WouldBlock) => {
                    let mut record = String::new();
                    file.read_to_string(&mut record)?;
                    let values = record
                        .split_whitespace()
                        .map(str::parse::<u64>)
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(io::Error::other)?;
                    let [output, x, y, width, height] = values.as_slice() else {
                        return Err(io::Error::other("Invalid pin placement"));
                    };
                    if *output == self.output {
                        occupied.push((*x, *y, *width, *height));
                    }
                }
                Err(TryLockError::Error(error)) => return Err(error),
            }
        }
        let mut columns = BTreeSet::from([u64::from(GAP)]);
        let mut rows = BTreeSet::from([u64::from(GAP)]);
        for &(x, y, width, height) in &occupied {
            columns.insert(if anchor.contains(Anchor::RIGHT) {
                u64::from(self.area.width)
                    .saturating_sub(x)
                    .saturating_add(u64::from(GAP))
            } else {
                x.saturating_add(width).saturating_add(u64::from(GAP))
            });
            rows.insert(if anchor.contains(Anchor::BOTTOM) {
                u64::from(self.area.height)
                    .saturating_sub(y)
                    .saturating_add(u64::from(GAP))
            } else {
                y.saturating_add(height).saturating_add(u64::from(GAP))
            });
        }
        let max_x = u64::from(self.area.width.saturating_sub(self.width + GAP));
        let max_y = u64::from(self.area.height.saturating_sub(self.height + GAP));
        let mut candidates = Vec::new();
        for offset_y in rows {
            for &offset_x in &columns {
                if offset_x > max_x || offset_y > max_y {
                    continue;
                }
                let (offset_x, offset_y) = (offset_x as u32, offset_y as u32);
                let x = if anchor.contains(Anchor::RIGHT) {
                    self.area.width - self.width - offset_x
                } else {
                    offset_x
                };
                let y = if anchor.contains(Anchor::BOTTOM) {
                    self.area.height - self.height - offset_y
                } else {
                    offset_y
                };
                candidates.push((x, y));
            }
        }
        if let Some((near_x, near_y)) = near {
            candidates.sort_by_key(|&(x, y)| {
                let dx = u64::from(x.abs_diff(near_x));
                let dy = u64::from(y.abs_diff(near_y));
                (dx + dy, dy)
            });
        }
        for (x, y) in candidates {
            let size = u64::from(self.width + GAP);
            if occupied.iter().any(|&(other_x, other_y, width, height)| {
                u64::from(x) < other_x.saturating_add(width).saturating_add(u64::from(GAP))
                    && other_x < u64::from(x) + size
                    && u64::from(y)
                        < other_y
                            .saturating_add(height)
                            .saturating_add(u64::from(GAP))
                    && other_y < u64::from(y) + u64::from(self.height + GAP)
            }) {
                continue;
            }
            let margin = IcedMargin {
                top: y as i32,
                right: (self.area.width - x - self.width) as i32,
                bottom: (self.area.height - y - self.height) as i32,
                left: x as i32,
            };
            if margin.left != self.margin.left
                || margin.right != self.margin.right
                || margin.top != self.margin.top
                || margin.bottom != self.margin.bottom
            {
                self.lease.rewind()?;
                self.lease.set_len(0)?;
                writeln!(
                    self.lease,
                    "{} {x} {y} {} {}",
                    self.output, self.width, self.height
                )?;
                self.margin = margin;
            }
            return Ok(margin);
        }
        Err(io::Error::other("No room for another shelf"))
    }
}

fn open(path: PathBuf) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
}
