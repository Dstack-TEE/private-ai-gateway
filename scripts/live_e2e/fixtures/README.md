`transcription.wav` is a synthetic, mono, 16 kHz PCM recording of:

> The quick brown fox jumps over the lazy dog.

Generated locally with FFmpeg's libflite `slt` voice (94,958 bytes):

```sh
ffmpeg -nostdin -f lavfi \
  -i 'flite=text=The quick brown fox jumps over the lazy dog.:voice=slt' \
  -ar 16000 -ac 1 -c:a pcm_s16le transcription.wav
```

The live test checks recognizable words, positive provider duration, JSON
allowlists, receipt hashes, and the serving attested session. No real user's
voice or audio is included.
