"""Canonical tree hash: SHA-256 over (relative path, file bytes) of every regular file, sorted by
path, with the stated exclusions. Prints the hash, the file count and the byte total."""
import hashlib, os, sys
EXCLUDE_DIRS = {".git", "__pycache__", "node_modules", ".pytest_cache", ".venv", "venv"}
EXCLUDE_FILES = {".DS_Store", "Thumbs.db"}
EXCLUDE_SUFFIX = (".pyc",)

def tree_hash(root):
    h = hashlib.sha256(); n = 0; total = 0; symlinks = 0
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        for d in list(dirnames):
            if os.path.islink(os.path.join(dirpath, d)):
                symlinks += 1; dirnames.remove(d)
        dirnames[:] = sorted(d for d in dirnames if d not in EXCLUDE_DIRS)
        for f in sorted(filenames):
            if f in EXCLUDE_FILES or f.endswith(EXCLUDE_SUFFIX):
                continue
            p = os.path.join(dirpath, f)
            if os.path.islink(p):
                symlinks += 1; continue
            rel = os.path.relpath(p, root).replace(os.sep, "/")
            data = open(p, "rb").read()
            h.update(rel.encode("utf-8") + b"\0" + hashlib.sha256(data).digest())
            n += 1; total += len(data)
    return h.hexdigest(), n, total, symlinks


if __name__ == "__main__":
    d, n, t, s = tree_hash(sys.argv[1])
    print(f"{d} files={n} bytes={t} symlinks={s}")
