/** Plays the 16-bit PCM an RDP server sends, queued back to back. */
export class PcmPlayer {
  private ctx: AudioContext | null = null;
  private next = 0;
  muted = false;

  /** Audio that would play this far ahead is dropped, so a stall doesn't
   *  turn into a growing delay. */
  private static readonly MAX_AHEAD = 0.5;

  play(channels: number, rate: number, pcm: Uint8Array) {
    if (this.muted || channels < 1 || channels > 2) return;
    const frames = Math.floor(pcm.length / (2 * channels));
    if (frames === 0) return;
    const ctx = (this.ctx ??= new AudioContext());
    void ctx.resume(); // allowed once the user has touched the page
    const buffer = ctx.createBuffer(channels, frames, rate);
    for (let c = 0; c < channels; c++) {
      const out = buffer.getChannelData(c);
      for (let i = 0; i < frames; i++) {
        const at = (i * channels + c) * 2;
        out[i] = ((pcm[at] | (pcm[at + 1] << 8)) << 16 >> 16) / 32768; // sign-extend 16 bits
      }
    }
    const now = ctx.currentTime;
    if (this.next < now + 0.02) this.next = now + 0.06; // ran dry: leave a little slack
    if (this.next - now > PcmPlayer.MAX_AHEAD) return;
    const src = ctx.createBufferSource();
    src.buffer = buffer;
    src.connect(ctx.destination);
    src.start(this.next);
    this.next += buffer.duration;
  }

  /** Let go of the audio device. */
  close() {
    void this.ctx?.close();
    this.ctx = null;
    this.next = 0;
  }
}
