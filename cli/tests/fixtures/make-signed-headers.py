# make-signed-headers.py SIGNED.exe: writes signed-headers.exe and its
# .thumbprint next to this script, the fixture cert.rs's tests read.
#
# A signed shell is ~800 KB, nearly all code the tests don't look at, so
# the fixture is just its PE headers plus its certificate table, with the
# table's file offset patched to where it now sits. Not runnable, but
# everything up to the signature parses as the real thing.
import hashlib, pathlib, struct, subprocess, sys

pe = pathlib.Path(sys.argv[1]).read_bytes()
hdr = struct.unpack_from("<I", pe, 0x3C)[0]
opt = hdr + 24
assert struct.unpack_from("<H", pe, opt)[0] == 0x20B, "PE32+ expected"
size_of_headers = struct.unpack_from("<I", pe, opt + 60)[0]
entry = opt + 112 + 8 * 4
offset, size = struct.unpack_from("<II", pe, entry)
out = bytearray(pe[:size_of_headers])
struct.pack_into("<I", out, entry, len(out))
out += pe[offset : offset + size]

here = pathlib.Path(__file__).parent
(here / "signed-headers.exe").write_bytes(out)
der = subprocess.run(
    ["openssl", "pkcs7", "-inform", "der", "-print_certs"], input=pe[offset + 8 : offset + size], capture_output=True, check=True
).stdout
der = subprocess.run(["openssl", "x509", "-outform", "der"], input=der, capture_output=True, check=True).stdout
(here / "signed-headers.thumbprint").write_text(hashlib.sha1(der).hexdigest().upper() + "\n")
