#!/usr/bin/env python3
"""Collect license and notice files from the resolved Cargo source graph."""
import json
import sys
from pathlib import Path

SAFE_NAMES = ('license', 'licence', 'copying', 'copyright', 'notice', 'unlicense', 'ofl')

# The crates.io archives for these versions omit their upstream notices.
PINNED = {
    *((name, '0.33.3') for name in (
        'ecolor', 'eframe', 'egui', 'egui-wgpu', 'egui-winit', 'egui_glow',
        'emath', 'epaint', 'epaint_default_fonts',
    )),
    ('gl_generator', '0.14.0'), ('khronos_api', '3.1.0'),
    ('hexf-parse', '0.2.1'), ('profiling', '1.0.18'),
    *((name, '0.5.5') for name in (
        'symphonia', 'symphonia-bundle-flac', 'symphonia-bundle-mp3', 'symphonia-core',
        'symphonia-metadata', 'symphonia-utils-xiph',
    )),
}
print(Path(__file__).with_name('upstream-notices.txt').read_text(encoding='utf-8'))


packages = json.load(sys.stdin)['packages']
for package in sorted(packages, key=lambda p: (p['name'], p['version'])):
    if package['source'] is None:
        continue
    root = Path(package['manifest_path']).parent
    names = [p for p in root.iterdir() if p.is_file() and p.name.lower().startswith(SAFE_NAMES)]
    key = (package['name'], package['version'])
    if key == ('epaint_default_fonts', '0.33.3'):
        names.extend((root / 'fonts').glob('*.txt'))
    if package['license_file'] is not None:
        names.append(root / package['license_file'])
    if not names:
        if key in PINNED:
            continue
        sys.exit(f"No license notice found for {package['name']} {package['version']}")
    print(f"\n=== {package['name']} {package['version']} ({package['license'] or 'see notices'}) ===")
    for notice in sorted(set(names)):
        print(f'--- {notice.name} ---')
        print(notice.read_text(encoding='utf-8-sig'))
