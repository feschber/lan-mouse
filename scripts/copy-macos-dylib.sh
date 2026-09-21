#!/bin/bash
set -eu

homebrew_path=""
exec_path="target/debug/bundle/osx/Lan Mouse.app/Contents/MacOS/lan-mouse"

usage() {
    cat <<EOF
$0: Copy all Homebrew libraries into the macOS app bundle.
USAGE: $0 [-h] [-b homebrew_path] [exec_path]

OPTIONS:
  -h, --help    Show this help message and exit
  -b            Path to Homebrew installation
                (default: obtained from 'brew --prefix')
  exec_path     Path to the main executable in the app bundle
                (default: $exec_path)

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
        -b | --homebrew )
            if [ $# -lt 2 ]; then
                echo "$0: $1 requires an argument" >&2
                exit 1
            fi
            homebrew_path="$2"; shift 2;;
        -* ) echo "$0: unknown option: $1" >&2; usage >&2; exit 1;;
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
# Path to bundled GTK/GSettings data
resources_path="$bundle_path/Contents/Resources"
share_path="$resources_path/share"

# Copy a library into the Frameworks directory unless it is already there, give
# it an install name of @rpath/<base_name> and recursively process its own
# dependencies.
#
# Usage: bundle_lib <base_name> <source_path>
# Returns non-zero (after warning) if <source_path> does not exist.
bundle_lib() {
  local base_name="$1"
  local source_path="$2"
  local dest="$fwks_path/$base_name"

  # Already bundled (this also covers a dylib's own install name, which shows
  # up in its `otool -L` output).
  if [ -e "$dest" ]; then
    return 0
  fi

  if [ ! -e "$source_path" ]; then
    echo "Warning: Could not find $base_name at $source_path" >&2
    return 1
  fi

  echo "Copying $source_path -> $dest"
  cp -f "$source_path" "$dest"
  # Ensure the copied dylib is writable so that xattr -rd /path/to/Lan\ Mouse.app works.
  chmod 644 "$dest"

  echo "Updating $dest to have install_name of @rpath/$base_name..."
  install_name_tool -id "@rpath/$base_name" "$dest"

  # Recursively process this dylib
  fix_references "$dest"

  return 0
}

# Copy and fix references for a binary (executable or dylib)
#
# This function will:
# - Copy any referenced dylibs from the Homebrew prefix to the Frameworks directory
# - Update the binary to reference the local copy instead
# - Recursively process the copied dylibs
#
# The Frameworks directory is added to the RPATH of the main executable only
# (see the end of this script); dyld resolves @rpath using the RPATHs of every
# binary in the load chain, so the bundled dylibs do not need their own.
fix_references() {
  local bin="$1"
  local libs rpath_libs loader_libs

  # Get all Homebrew libraries referenced by the binary (absolute paths)
  libs=$(otool -L "$bin" | awk -v homebrew="$homebrew_path" 'index($1, homebrew) == 1 {print $1}')

  # Also get @rpath/@loader_path references and try to resolve them from Homebrew
  rpath_libs=$(otool -L "$bin" | awk '$1 ~ /^@rpath\// {print $1}')
  loader_libs=$(otool -L "$bin" | awk '$1 ~ /^@loader_path\// {print $1}')

  echo "$libs" | while IFS= read -r old_path; do
    if [ -z "$old_path" ]; then
      continue
    fi

    local base_name="$(basename "$old_path")"

    bundle_lib "$base_name" "$old_path"

    echo "Updating $bin to reference @rpath/$base_name..."
    install_name_tool -change "$old_path" "@rpath/$base_name" "$bin"
  done

  # Process @rpath references. These stay as they are -- they resolve to the
  # Frameworks directory via the RPATH of the loading binary -- so the library
  # only needs to be copied there.
  echo "$rpath_libs" | while IFS= read -r rpath_ref; do
    if [ -z "$rpath_ref" ]; then
      continue
    fi

    local base_name="$(basename "$rpath_ref")"

    bundle_lib "$base_name" "$homebrew_path/lib/$base_name" || true
  done

  # Process @loader_path references. Unlike @rpath, these resolve relative to
  # the directory of the *loading* binary, which is wrong once the library has
  # been moved into Frameworks, so rewrite them to @rpath.
  echo "$loader_libs" | while IFS= read -r loader_ref; do
    if [ -z "$loader_ref" ]; then
      continue
    fi

    local base_name="$(basename "$loader_ref")"

    if bundle_lib "$base_name" "$homebrew_path/lib/$base_name"; then
      echo "Updating $bin to reference @rpath/$base_name..."
      install_name_tool -change "$loader_ref" "@rpath/$base_name" "$bin"
    fi
  done
}

fix_references "$exec_path"

copy_runtime_data() {
  mkdir -p "$share_path"

  if [ -d "$homebrew_path/share/glib-2.0/schemas" ]; then
    mkdir -p "$share_path/glib-2.0"
    rm -rf "$share_path/glib-2.0/schemas"
    cp -RL "$homebrew_path/share/glib-2.0/schemas" "$share_path/glib-2.0/schemas"
    if command -v glib-compile-schemas >/dev/null 2>&1; then
      glib-compile-schemas "$share_path/glib-2.0/schemas"
    elif [ -x "$homebrew_path/bin/glib-compile-schemas" ]; then
      "$homebrew_path/bin/glib-compile-schemas" "$share_path/glib-2.0/schemas"
    fi
  fi

  if [ -d "$homebrew_path/share/gtk-4.0" ]; then
    rm -rf "$share_path/gtk-4.0"
    cp -RL "$homebrew_path/share/gtk-4.0" "$share_path/gtk-4.0"
  fi

  if [ -d "$homebrew_path/share/icons/Adwaita" ]; then
    mkdir -p "$share_path/icons"
    rm -rf "$share_path/icons/Adwaita"
    cp -RL "$homebrew_path/share/icons/Adwaita" "$share_path/icons/Adwaita"
  fi
}

copy_runtime_data

# cargo-bundle preserves the source path under Contents/Resources (so
# `target/menubar-template.png` lands at `Resources/target/...`). Flatten it
# so NSBundle pathForResource: finds the file at the Resources root.
if [ -f "$resources_path/target/menubar-template.png" ]; then
  mv "$resources_path/target/menubar-template.png" "$resources_path/menubar-template.png"
  rmdir "$resources_path/target" 2>/dev/null || true
fi

# Ensure the main executable has our Frameworks path in its RPATH
if ! otool -l "$exec_path" | grep -qF "@executable_path/../Frameworks"; then
  echo "Adding RPATH to $exec_path"
  install_name_tool -add_rpath "@executable_path/../Frameworks" "$exec_path"
fi

# Sign the .app. Nested code has to be signed inside-out: sign the bundled
# libraries first, then the bundle itself (`codesign --deep` is deprecated).
find "$fwks_path" -name '*.dylib' -exec codesign --force --sign - {} +
codesign --force --sign - "$bundle_path"

echo "Done!"
