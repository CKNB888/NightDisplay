"""Download the pinned upstream driver for source/CI builds."""
import argparse
import hashlib
import io
import urllib.request
import zipfile
from pathlib import Path

URL = "https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.7.23/VirtualDisplayDriver-x86.Driver.Only.zip"
SHA256 = "e24210692b442b39af763536330ce78b423f19342b7a7792c26de3944e418b3a"


def prepare(destination, archive=None):
    if archive:
        data = Path(archive).read_bytes()
    else:
        with urllib.request.urlopen(URL, timeout=60) as response:
            data = response.read(10 * 1024 * 1024 + 1)
    if hashlib.sha256(data).hexdigest() != SHA256:
        raise RuntimeError("VDD archive checksum mismatch")
    destination = Path(destination).resolve()
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for info in z.infolist():
            target = (destination / info.filename).resolve()
            if destination != target and destination not in target.parents:
                raise RuntimeError("Unsafe driver archive path")
        z.extractall(destination)
    if not list(destination.rglob("MttVDD.inf")):
        raise RuntimeError("VDD archive has no INF")
    return destination


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--dest", default="vendor/vdd")
    parser.add_argument("--archive")
    args = parser.parse_args()
    print(prepare(args.dest, args.archive))
