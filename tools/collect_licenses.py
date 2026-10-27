"""Collect installed runtime dependency notices alongside packaged applications."""
import shutil
from importlib.metadata import PackageNotFoundError, distribution
from pathlib import Path


def collect(destination="third-party-licenses"):
    output = Path(destination)
    output.mkdir(parents=True, exist_ok=True)
    for name in ("pillow", "pystray", "six", "python-xlib", "pyobjc-core", "pyobjc-framework-Cocoa"):
        try:
            dist = distribution(name)
        except PackageNotFoundError:
            continue
        for file in dist.files or []:
            if any(key in file.name.lower() for key in ("license", "copying", "notice")):
                source = Path(dist.locate_file(file))
                if source.is_file():
                    folder = output / name
                    folder.mkdir(exist_ok=True)
                    shutil.copyfile(source, folder / file.name)
    shutil.copyfile("nightdisplay/resources/VDD-LICENSE.txt", output / "VDD-LICENSE.txt")
    return output


if __name__ == "__main__":
    print(collect())
