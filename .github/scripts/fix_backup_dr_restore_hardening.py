#!/usr/bin/env python3
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
path = ROOT / "tests/tests/restore_hardening.rs"
text = path.read_text()
old = '''                backup_ref: None,
            },
            restore_image:'''
new = '''                backup_ref: None,
                external_backup: None,
            },
            restore_image:'''
count = text.count(old)
if count != 1:
    raise RuntimeError(f"restore_hardening source initializer count: {count}")
path.write_text(text.replace(old, new, 1))
print("restore hardening beta source migration applied")
