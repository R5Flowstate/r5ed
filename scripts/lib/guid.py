"""RTech::StringToGuid -- the engine's asset-name hash, from Repak's
StringToGuidAligned. Every multiply wraps at 64 bits before its shift, as the
C code does."""
import struct

M32 = 0xFFFFFFFF
M64 = 0xFFFFFFFFFFFFFFFF


def string_to_guid(s):
    b = (s.encode("latin-1") if isinstance(s, str) else bytes(s)) + b"\0" * 8
    v1 = 0
    i = 0
    while True:
        w = struct.unpack_from("<I", b, i)[0]
        v4 = ~w & (w - 0x1010101) & 0x80808080 & M32
        v5 = (v4 ^ (v4 - 1)) & M32
        v6 = ((v5 & w) ^ 0x5C5C5C5C) & M32
        v7 = ~v6 & (v6 - 0x1010101) & 0x80808080 & M32
        v8 = v7 & (-v7 & M32)
        if v7 != v8:
            v9 = 0xFF000000
            while True:
                v10 = v9
                if (v9 & v6) == 0:
                    v8 |= v9 & 0x80808080
                v9 >>= 8
                if v10 < 0x100:
                    break
        v11 = (0x633D5F1 * v1) & M64
        v12 = ((0xFB8C4D96501 * (((v5 & w) - 45 * (v8 >> 7)) & 0xDFDFDFDF)) & M64) >> 24
        if v4:
            break
        i += 4
        s64 = (v11 + v12) & M64
        v1 = (s64 >> 61) ^ s64
    v13 = v5.bit_length() - 1 if v5 else -1
    return (v12 + v11 - 0xAE502812AA7333 * ((i + v13 // 8) & M32)) & M64


if __name__ == "__main__":
    import sys
    for a in sys.argv[1:]:
        print("0x%016X  %s" % (string_to_guid(a), a))
