#!/bin/sh
set -e

usage() {
    cat <<EOF
$0: Make a macOS icns file from an SVG with ImageMagick and iconutil.
usage: $0 [SVG [ICNS [ICONSET]]

ARGUMENTS
    SVG     The SVG file to convert
            Defaults to ./lan-mouse-gtk/resources/de.feschber.LanMouse.svg
    ICNS    The icns file to create
            Defaults to ./target/icon.icns
    ICONSET The iconset directory to create
            Defaults to ./target/icon.iconset
            This is just a temporary directory
EOF
}

if [ "$1" = "-h" ] || [ "$1" = "--help" ]; then
    usage
    exit 0
fi

svg="${1:-./lan-mouse-gtk/resources/de.feschber.LanMouse.svg}"
icns="${2:-./target/icon.icns}"
iconset="${3:-./target/icon.iconset}"

set -u

mkdir -p "$iconset"
magick "$svg" -background none -resize 1024x1024 "$iconset"/icon_512x512@2x.png
magick "$svg" -background none -resize 512x512 "$iconset"/icon_512x512.png
magick "$svg" -background none -resize 256x256 "$iconset"/icon_256x256.png
magick "$svg" -background none -resize 128x128 "$iconset"/icon_128x128.png
magick "$svg" -background none -resize 64x64 "$iconset"/icon_32x32@2x.png
magick "$svg" -background none -resize 32x32 "$iconset"/icon_32x32.png
magick "$svg" -background none -resize 16x16 "$iconset"/icon_16x16.png
cp "$iconset"/icon_512x512.png "$iconset"/icon_256x256@2x.png
cp "$iconset"/icon_256x256.png "$iconset"/icon_128x128@2x.png
cp "$iconset"/icon_32x32.png "$iconset"/icon_16x16@2x.png
iconutil -c icns "$iconset" -o "$icns"
