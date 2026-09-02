"""Canonical tree hash: SHA-256 over (relative path, file bytes) of every regular file, sorted by
path, with the stated exclusions. Symlinks and special nodes are counted but never opened."""
import hashlib, os, stat, sys
EXCLUDE_DIRS = {".git", "__pycache__", "node_modules", ".pytest_cache", ".venv", "venv"}
EXCLUDE_FILES = {".DS_Store", "Thumbs.db"}
EXCLUDE_SUFFIX = (".pyc",)

def tree_hash(root):
    if not stat.S_ISDIR(os.lstat(root).st_mode):
        raise ValueError("artifact root is not a directory")
    h = hashlib.sha256(); n = 0; total = 0; symlinks = 0; specials = 0
    for dirpath, dirnames, filenames in os.walk(root, followlinks=False):
        rel_dir = os.path.relpath(dirpath, root)
        excluded_tree = rel_dir != "." and any(part in EXCLUDE_DIRS for part in rel_dir.split(os.sep))
        for d in list(dirnames):
            p = os.path.join(dirpath, d)
            mode = os.lstat(p).st_mode
            if stat.S_ISLNK(mode):
                symlinks += 1; dirnames.remove(d)
            elif not stat.S_ISDIR(mode):
                specials += 1; dirnames.remove(d)
        # Traverse excluded directories too so unsafe nodes hidden inside them are still found;
        # their regular files remain excluded from the digest below.
        dirnames.sort()
        for f in sorted(filenames):
            p = os.path.join(dirpath, f)
            mode = os.lstat(p).st_mode
            if stat.S_ISLNK(mode):
                symlinks += 1; continue
            if not stat.S_ISREG(mode):
                specials += 1; continue
            if excluded_tree or f in EXCLUDE_FILES or f.endswith(EXCLUDE_SUFFIX):
                continue
            rel = os.path.relpath(p, root).replace(os.sep, "/")
            data = open(p, "rb").read()
            h.update(rel.encode("utf-8") + b"\0" + hashlib.sha256(data).digest())
            n += 1; total += len(data)
    return h.hexdigest(), n, total, symlinks, specials


if __name__ == "__main__":
    d, n, t, links, specials = tree_hash(sys.argv[1])
    print(f"{d} files={n} bytes={t} symlinks={links} specials={specials}")
