#!/bin/sh
set -e

usage() {
    cat <<EOF
$0: Make a macOS icns file from an SVG with rsvg-convert, ImageMagick and iconutil.

Follows the Big Sur+ icon template:
  - 1024x1024 canvas with a rounded-square (squircle) background
  - Icon artwork scaled to fit inside an 824x824 content area, centered
  - Transparent padding outside the squircle so the Dock/Finder render it
    like other first-party macOS apps.

usage: $0 [SVG [ICNS [ICONSET]]]

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

workdir="$(dirname "$iconset")/icon-work"
rm -rf "$iconset" "$workdir"
mkdir -p "$iconset" "$workdir"

# Big Sur+ macOS icon template proportions (in a 1024 canvas):
#   canvas  = 1024
#   squircle = 824  (the white rounded-square background, inset 100px)
#   content  = 560  (artwork inside the squircle, with generous margin)
#   radius   = 185  (~22.5% of the squircle, the characteristic curvature)
CANVAS=1024
SQUIRCLE=824
CONTENT=560
RADIUS=185
BG_COLOR="#FFFFFF"
SQUIRCLE_OFFSET=$(( (CANVAS - SQUIRCLE) / 2 ))
CONTENT_OFFSET=$(( (CANVAS - CONTENT) / 2 ))

# 1) Render the SVG to the content size at full fidelity.
#    rsvg-convert handles our SVG correctly; ImageMagick sometimes crops it.
rsvg-convert -w "$CONTENT" -h "$CONTENT" "$svg" -o "$workdir/content.png"

# 2) Draw the rounded-square (squircle) background on a transparent canvas.
#    The squircle is inset from the canvas edges (transparent padding), so the
#    Dock/Finder render it at the same visual size as other first-party apps.
magick -size ${CANVAS}x${CANVAS} xc:none \
    -fill "$BG_COLOR" \
    -draw "roundrectangle ${SQUIRCLE_OFFSET},${SQUIRCLE_OFFSET} $((CANVAS-SQUIRCLE_OFFSET-1)),$((CANVAS-SQUIRCLE_OFFSET-1)) $RADIUS,$RADIUS" \
    "$workdir/background.png"

# 3) Composite the artwork onto the background, centered inside the content area.
magick "$workdir/background.png" \
    "$workdir/content.png" -geometry +${CONTENT_OFFSET}+${CONTENT_OFFSET} -composite \
    "$workdir/icon-1024.png"

# 4) Generate each iconset size from the master so all sizes share the same
#    squircle proportions and look consistent at every resolution.
for size in 1024 512 256 128 64 32 16; do
    magick "$workdir/icon-1024.png" -resize ${size}x${size} "$workdir/${size}.png"
done

cp "$workdir/1024.png" "$iconset"/icon_512x512@2x.png
cp "$workdir/512.png"  "$iconset"/icon_512x512.png
cp "$workdir/512.png"  "$iconset"/icon_256x256@2x.png
cp "$workdir/256.png"  "$iconset"/icon_256x256.png
cp "$workdir/256.png"  "$iconset"/icon_128x128@2x.png
cp "$workdir/128.png"  "$iconset"/icon_128x128.png
cp "$workdir/64.png"   "$iconset"/icon_32x32@2x.png
cp "$workdir/32.png"   "$iconset"/icon_32x32.png
cp "$workdir/32.png"   "$iconset"/icon_16x16@2x.png
cp "$workdir/16.png"   "$iconset"/icon_16x16.png

iconutil -c icns "$iconset" -o "$icns"
