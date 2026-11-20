#!/usr/bin/env python3
"""Install the pinned HTML/CSS/JSON servers or update their lockfile."""
import argparse
from pathlib import Path

from setup_language_tools import install_npm, require_node_version, setup_tool


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lock-only', action='store_true')
    if parser.parse_args().lock_only:
        require_node_version()
        install_npm(Path(__file__).resolve().parent / 'web-language-servers', lock_only=True)
    else:
        print(*setup_tool('web'), sep='\n')
