default: app

# Cargo.toml [package] version — single source for the bundle's version keys.
version := `grep -m1 '^version = ' Cargo.toml | sed 's/version = "\(.*\)"/\1/'`

build:
    cargo build --release

# Assemble build/Sonos.app: release binary + Info.plist, ad-hoc signed.
app: build
    rm -rf build/Sonos.app
    mkdir -p build/Sonos.app/Contents/MacOS build/Sonos.app/Contents/Resources
    cp target/release/sonos build/Sonos.app/Contents/MacOS/sonos
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
