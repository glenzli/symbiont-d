#!/usr/bin/env bash
set -euo pipefail

resource_dir="$(cd "$(dirname "$0")" && pwd)"
scratch_dir="$(mktemp -d "${TMPDIR:-/tmp}/symbiont-icons.XXXXXX")"
iconset_dir="$scratch_dir/AppIcon.iconset"

cleanup() {
  rm -rf "$scratch_dir"
}
trap cleanup EXIT

cp "$resource_dir/AppIconMaster.png" "$resource_dir/AppIconSource.png"
ffmpeg -v error -y -i "$resource_dir/AppIconMaster.png" \
  -vf "crop=650:760:300:230,format=rgba,colorkey=0xFEFEFE:0.08:0.04,scale=30:30:force_original_aspect_ratio=decrease:flags=lanczos,pad=36:36:(ow-iw)/2:(oh-ih)/2:color=0x00000000" \
  -frames:v 1 "$resource_dir/MenuBarIcon@2x.png"
sips -z 18 18 "$resource_dir/MenuBarIcon@2x.png" --out "$resource_dir/MenuBarIcon.png" >/dev/null

mkdir -p "$iconset_dir"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" "$resource_dir/AppIconSource.png" --out "$iconset_dir/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" "$resource_dir/AppIconSource.png" --out "$iconset_dir/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset_dir" -o "$resource_dir/AppIcon.icns"
