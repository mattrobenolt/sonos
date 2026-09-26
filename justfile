default: app

# Cargo.toml [package] version — single source for the bundle's version keys.
version := `grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/'`

build:
    cargo build --release

icon-url := "https://is1-ssl.mzstatic.com/image/thumb/Purple211/v4/3c/8a/a4/3c8aa417-5af9-475b-3983-5778efecce09/AppIcon-0-0-1x_U007epad-0-1-0-85-220.png/1024x1024bb.jpg"

# Regenerate misc/AppIcon.icns from the official Sonos app artwork (App
# Store CDN). sips ignores the output filename extension — force PNG or
# iconutil rejects the files (JPEG data wearing .png names).
icon:
    curl -sS -o /tmp/sonos-icon.png "{{icon-url}}"
    rm -rf /tmp/sonos.iconset && mkdir -p /tmp/sonos.iconset
    for size in 16 32 128 256 512; do \
      sips -s format png -z $size $size /tmp/sonos-icon.png --out /tmp/sonos.iconset/icon_${size}x${size}.png >/dev/null; \
      sips -s format png -z $((size*2)) $((size*2)) /tmp/sonos-icon.png --out /tmp/sonos.iconset/icon_${size}x${size}@2x.png >/dev/null; \
    done
    sips -s format png /tmp/sonos-icon.png --out /tmp/sonos.iconset/icon_512x512@2x.png >/dev/null
    env -u DEVELOPER_DIR -u SDKROOT /usr/bin/iconutil -c icns /tmp/sonos.iconset -o misc/AppIcon.icns

# Assemble build/Sonos.app: release binary + Info.plist + icon, ad-hoc signed.
app: build
    rm -rf build/Sonos.app
    mkdir -p build/Sonos.app/Contents/MacOS build/Sonos.app/Contents/Resources
    cp target/release/sonos build/Sonos.app/Contents/MacOS/sonos
    cp misc/AppIcon.icns build/Sonos.app/Contents/Resources/AppIcon.icns
    sed "s/__VERSION__/{{version}}/" misc/Info.plist > build/Sonos.app/Contents/Info.plist
    plutil -lint build/Sonos.app/Contents/Info.plist
    codesign --force --sign - build/Sonos.app
    @echo "built build/Sonos.app ({{version}})"

# Install into /Applications (idempotent).
install: app
    rm -rf /Applications/Sonos.app
    cp -R build/Sonos.app /Applications/Sonos.app
    @echo "installed /Applications/Sonos.app"

# Launch through Finder/LaunchServices, not cargo — exercises the bundle.
run: app
    open build/Sonos.app
