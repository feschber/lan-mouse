#!/bin/sh
set -eu

homebrew_path=""
exec_path="target/debug/bundle/osx/Lan Mouse.app/Contents/MacOS/lan-mouse"

usage() {
    cat <<EOF
$0: Copy all Homebrew libraries into the macOS app bundle.
USAGE: $0 [-h] [-b homebrew_path] [exec_path]

OPTIONS:
  -h, --help    Show this help message and exit
  -b            Path to Homebrew installation (default: $homebrew_path)
  exec_path     Path to the main executable in the app bundle
                (default: get from `brew --prefix`)

When macOS apps are linked to dynamic libraries (.dylib files),
the fully qualified path to the library is embedded in the binary.
If the libraries come from Homebrew, that means that Homebrew must be present
and the libraries must be installed in the same location on the user's machine.

This script copies all of the Homebrew libraries that an executable links to into the app bundle
and tells all the binaries in the bundle to look for them there.
EOF
}

# Gather command-line arguments
while test $# -gt 0; do
    case "$1" in
        -h | --help ) usage; exit 0;;
        -b | --homebrew ) homebrew_path="$1"; shift 2;;
        * ) exec_path="$1"; shift;;
    esac
done

if [ -z "$homebrew_path" ]; then
    homebrew_path="$(brew --prefix)"
fi

# Path to the .app bundle
bundle_path=$(dirname "$(dirname "$(dirname "$exec_path")")")
# Path to the Frameworks directory
fwks_path="$bundle_path/Contents/Frameworks"
mkdir -p "$fwks_path"

# Copy and fix references for a binary (executable or dylib)
#
# This function will:
# - Copy any referenced dylibs from /opt/homebrew to the Frameworks directory
# - Update the binary to reference the local copy instead
# - Add the Frameworks directory to the binary's RPATH
# - Recursively process the copied dylibs
fix_references() {
  local bin="$1"

  # Get all Homebrew libraries referenced by the binary
  libs=$(otool -L "$bin" | awk -v homebrew="$homebrew_path" '$0 ~ homebrew {print $1}')

  echo "$libs" | while IFS= read -r old_path; do
    local base_name="$(basename "$old_path")"
    local dest="$fwks_path/$base_name"

    if [ ! -e "$dest" ]; then
      echo "Copying $old_path -> $dest"
      cp -f "$old_path" "$dest"
      # Ensure the copied dylib is writable so that xattr -rd /path/to/Lan\ Mouse.app works.
      chmod 644 "$dest"

      echo "Updating $dest to have install_name of @rpath/$base_name..."
      install_name_tool -id "@rpath/$base_name" "$dest"

      # Recursively process this dylib
      fix_references "$dest"
    fi

    echo "Updating $bin to reference @rpath/$base_name..."
    install_name_tool -change "$old_path" "@rpath/$base_name" "$bin"
  done
}

fix_references "$exec_path"

# Ensure the main executable has our Frameworks path in its RPATH
if ! otool -l "$exec_path" | grep -q "@executable_path/../Frameworks"; then
  echo "Adding RPATH to $exec_path"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$exec_path"
fi

# Se-sign the .app
codesign --force --deep --sign - "$bundle_path"

echo "Done!"
