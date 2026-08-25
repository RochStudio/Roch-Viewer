# -*- mode: python ; coding: utf-8 -*-
# PyInstaller spec for Roch Viewer.
# Build with: py -m PyInstaller -y RochViewer.spec

from PyInstaller.utils.hooks import collect_data_files

# customtkinter needs its theme/data files collected.
datas = collect_data_files('customtkinter')

# Files used directly by the app at runtime.
#
# The low-level driver is deliberately not in this list. It is a third-party
# component under its own terms, and this project is GPL-3.0; bundling it
# would make the combined work something neither licence covers. Put
# inpoutx64.dll and inpoutx64.sys beside the built EXE yourself -- see the
# README's Prerequisites -- or beside main.py to run from source.
datas += [
    ('icon.ico', '.'),
]

hiddenimports = [
    'version',
    'customtkinter',
    'wmi',
    'win32com',
    'win32com.client',
    'pythoncom',
    'pywintypes',
    'platform_profiles',
    'timings',
    'intel_timings',
    # Imported lazily by intel_timings so the tables are built before a
    # sensor is read, which also hides these from the dependency scan.
    'intel_board_sensors',
    'intel_pch_smbus',
    'intel_rapl',
    'ddr4_tsod',
    'ddr5_pmic',
    'ddr5_spd',
    # Imported inside the Telemetry button handler, so the dependency scan
    # never sees either one.
    'ddr5_telemetry',
    'dimm_telemetry_window',
    'superio_lpc',
    # Imported inside the row getters that use them: the identity readings on
    # System Info, and the performance counters behind the Clocks section.
    'system_identity',
    'cpu_clocks',
    'unsupported_profile',
    'display_values',
    'dimm_inventory',
    'dram_ic',
    'lazy_read',
    'lowlevel_io',
    'pci_mcfg',
    'voltage_rails',
    # The card's own readings, its power limit and the error counter.
    #
    # Belt and braces, not a fix: PyInstaller scans bytecode and finds a
    # plain import inside a function body perfectly well. The entries here
    # cost nothing and say what the build is expected to carry, which
    # test_packaging then holds it to.
    'nvidia_gpu',
    'whea_errors',
]

a = Analysis(
    ['main.py'],
    pathex=[],
    binaries=[],
    datas=datas,
    hiddenimports=hiddenimports,
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='RochViewer',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon='icon.ico',
    uac_admin=True,
    version='file_version_info.txt',
)
