#!/bin/sh
set -eu
python3 ../tools/prepare-frontend-startup.py
sh pwa/build.sh
