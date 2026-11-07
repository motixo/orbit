#!/bin/sh
exec flatpak-spawn --host --watch-bus "$(basename "$0")" "$@"
