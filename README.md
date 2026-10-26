# Soundboard
NOTE: This project has been vibecoded, so dont expect it to be great..

A small, portable Windows soundboard that plays sounds into your microphone.

- Single `soundboard.exe`, no installer, no runtime to install
- Drag & drop audio files onto the board (mp3 / wav / ogg / flac / m4a / aac)
- Overlapping playback with per-pad volume, loop, rename and remove
- Global hotkeys: `Ctrl+Alt+1` ... `Ctrl+Alt+0`
- Output device picker, master volume, stop-all
- **Virtual mic setup**: downloads and installs VB-Audio Cable from the official source with one UAC prompt
- **PC audio**: routes everything playing on a chosen device into the mic (WASAPI loopback)
- Local sound library: index folders in place, instant search, `.zip` pack import
- Everything portable: config and sounds live next to the exe

## Download

Grab the [latest release](https://github.com/neonnathan/soundboard/releases/latest), unzip it anywhere and run `soundboard.exe`.

## Setup

1. Run `soundboard.exe`
2. No virtual mic yet? Click **Set up virtual mic** and approve the admin prompt
3. Set **Output** to `CABLE Input (VB-Audio Virtual Cable)`
4. In Discord / OBS / your game, set the microphone to `CABLE Output (VB-Audio Virtual Cable)`
5. Optional: turn on **PC audio** to send whatever is playing on your PC to the mic too

### PC audio caveat

Loopback captures everything on the selected device, including Discord's own output, which can echo back to other people. Keep Discord on a different output than the one you capture, or mix with VoiceMeeter.

## Build

Needs Rust and the MSVC build tools. The default GNU toolchain cannot link this project:

```
cargo +stable-x86_64-pc-windows-msvc build --release
```

The binary lands in `target/release/soundboard.exe`.

## Logo

The in-app and window icon is loaded from `assets/logo.png` next to the exe at runtime. The exe file icon is embedded from `assets/logo.ico` at build time.

## License

MIT
