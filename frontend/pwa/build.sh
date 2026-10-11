#!/bin/sh
set -eu
dist=${TRUNK_STAGING_DIR:?Trunk staging directory is required}
for file in manifest.webmanifest pwa.js offline.html logo.svg favicon.svg favicon.png apple-touch-icon.png icon-192.png icon-512.png maskable-icon-192.png maskable-icon-512.png; do
  cp "pwa/$file" "$dist/$file"
done
python3 ../tools/open_source.py --page-output "$dist/about.html"
# Reuse the exact frontend build in a dedicated Rust/WASM module worker.
module=$(find "$dist" -maxdepth 1 -name 'openwebide-frontend-*.js' -type f)
[ -n "$module" ] && [ -f "$module" ]
wasm="${module%.js}_bg.wasm"
[ -f "$wasm" ]
sed -e "s/__EDITOR_MODULE__/${module##*/}/g" -e "s/__EDITOR_WASM__/${wasm##*/}/g" pwa/editor-worker.js > "$dist/editor-worker.js"
fingerprint=${module##*/}
fingerprint=${fingerprint#openwebide-frontend-}
fingerprint=${fingerprint%.js}
cp "$dist/editor-worker.js" "$dist/editor-worker-$fingerprint.js"
version=$({ cksum "$dist/index.html" "$dist/pwa.js" "$dist/manifest.webmanifest" "$dist/offline.html" "$dist/about.html" "$dist"/about-software-*.html "$dist/logo.svg" "$dist/favicon.svg" "$dist/favicon.png" "$dist/apple-touch-icon.png" "$dist/icon-192.png" "$dist/icon-512.png" "$dist/maskable-icon-192.png" "$dist/maskable-icon-512.png" pwa/service-worker.js pwa/build.sh pwa/editor-worker.js; find "$dist/fonts" -type f -name '*.woff2' -exec cksum {} \; | sort; } | cksum | cut -d ' ' -f 1)
{
  printf 'const BUILD_ID = "%s";\nconst SHELL_FILES = ["/", "/offline.html", "/about.html", "/manifest.webmanifest", "/logo.svg", "/favicon.svg", "/favicon.png", "/apple-touch-icon.png", "/icon-192.png", "/icon-512.png", "/maskable-icon-192.png", "/maskable-icon-512.png", "/pwa.js"' "$version"
  for file in "$dist"/*.wasm "$dist"/*.js "$dist"/*.css "$dist"/about-software-*.html; do
    [ -f "$file" ] || continue
    name=${file##*/}
    [ "$name" = pwa.js ] && continue
    [ "$name" = service-worker.js ] && continue
    printf ',"/%s"' "$name"
  done
  find "$dist/fonts" -type f -name '*.woff2' | sort | while IFS= read -r file; do
    printf ',"/%s"' "${file#"$dist"/}"
  done
  if [ -d "$dist/snippets" ]; then
    find "$dist/snippets" -type f -name '*.js' | sort | while IFS= read -r file; do
      printf ',"/%s"' "${file#"$dist"/}"
    done
  fi
  printf '];\n'
  cat pwa/service-worker.js
} > "$dist/service-worker.js"
