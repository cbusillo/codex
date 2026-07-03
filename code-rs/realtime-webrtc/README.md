# Realtime WebRTC

This crate keeps the TUI-facing WebRTC transport API available without pulling a
native WebRTC implementation into the default Every Code build.

The previous macOS native implementation depended on `libwebrtc` from
`juberti-oai/rust-sdks`, whose nested git submodules could stall `cargo
metadata` before `./build-fast.sh` reached compilation. Until native WebRTC is
restored as an explicit overlay, `RealtimeWebrtcSession::start()` returns
`UnsupportedPlatform` and websocket realtime remains the supported default
transport.
