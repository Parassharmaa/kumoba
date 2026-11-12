import os
import sys
from pathlib import Path

root = Path(sys.argv[1])
for directory, directories, files in os.walk(root, followlinks=False):
    for name in directories + files:
        path = "/" + (Path(directory) / name).relative_to(root).as_posix()
        if any(character in path for character in ['\n', '\r', '\x00']):
            raise ValueError("rootfs paths cannot contain control characters")
        quoted = '"' + path.replace('\\', '\\\\').replace('"', '\\"') + '"'
        print(f"set_inode_field {quoted} uid 0")
        print(f"set_inode_field {quoted} gid 0")
