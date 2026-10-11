// 生成 C96 GUI 的应用图标（无第三方依赖）。
// 用法: node tools/make_icon.mjs icons/icon.ico
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";

const out = process.argv[2] || "icons/icon.ico";
mkdirSync(dirname(out), { recursive: true });

const W = 64, H = 64;
const xor = Buffer.alloc(W * H * 4);
for (let y = 0; y < H; y++) {
  for (let x = 0; x < W; x++) {
    const dx = x - (W - 1) / 2, dy = y - (H - 1) / 2;
    const d = Math.sqrt(dx * dx + dy * dy);
    let r, g, b;
    if (d > 27) { r = 0x1e; g = 0x25; b = 0x30; }
    else if (d > 16) { r = 0x2f; g = 0x6f; b = 0xed; }
    else if (d > 8) { r = 0xe6; g = 0xee; b = 0xf6; }
    else { r = 0x2f; g = 0x6f; b = 0xed; }
    const row = H - 1 - y;            // ICO/BMP 像素自底向上
    const o = (row * W + x) * 4;
    xor[o] = b; xor[o + 1] = g; xor[o + 2] = r; xor[o + 3] = 0xff;
  }
}
const andRowBytes = Math.ceil(W / 32) * 4;   // 1bpp，行按 4 字节对齐
const and = Buffer.alloc(andRowBytes * H, 0);

const header = Buffer.alloc(40);
header.writeUInt32LE(40, 0);
header.writeInt32LE(W, 4);
header.writeInt32LE(H * 2, 8);
header.writeUInt16LE(1, 12);
header.writeUInt16LE(32, 14);
header.writeUInt32LE(0, 16);
header.writeUInt32LE(xor.length + and.length, 20);

const image = Buffer.concat([header, xor, and]);
const dir = Buffer.alloc(6);
dir.writeUInt16LE(0, 0); dir.writeUInt16LE(1, 2); dir.writeUInt16LE(1, 4);
const entry = Buffer.alloc(16);
entry[0] = W; entry[1] = H; entry[2] = 0; entry[3] = 0;
entry.writeUInt16LE(1, 4); entry.writeUInt16LE(32, 6);
entry.writeUInt32LE(image.length, 8); entry.writeUInt32LE(6 + 16, 12);

writeFileSync(out, Buffer.concat([dir, entry, image]));
console.log("wrote " + out + " (" + (6 + 16 + image.length) + " bytes)");
