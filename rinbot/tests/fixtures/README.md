# SILK compatibility fixture

`silk-v0.2-triangle.silk` contains 100 ms of synthetic mono 16 kHz audio,
not a recorded voice. It was encoded with crates.io `silk-rs = 0.2.0`:

```rust
let pcm: Vec<u8> = (0_i32..1600).flat_map(|i| {
    let phase = i % 80;
    let sample = if phase < 40 { phase * 400 - 8000 } else { 24000 - phase * 400 };
    (sample as i16).to_le_bytes()
}).collect();
let encoded = silk_rs::encode_silk(&pcm, 16000, 24000, true).unwrap();
let decoded = silk_rs::decode_silk(&encoded, 16000).unwrap();
```

The original decoder produces 3,200 PCM bytes with SHA-256
`ae238ad80ffc1c447aeafe02639ee382ab909375b0f7f3416733090018cf462f`.
The encoded file has 230 bytes with SHA-256
`65b7d613c8b9abbf0dec288417a82bad2c31c9dc3f69eab672595ae7b103c85b`.

The ASR regression checks that both QQ's `0x02` prefix and the plain SILK
header decode to the legacy output exactly, and that truncated framing is
rejected before entering the native decoder.
