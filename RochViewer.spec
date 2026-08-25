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
    'rochviewer.version',
    'customtkinter',
    'wmi',
    'win32com',
    'win32com.client',
    'pythoncom',
    'pywintypes',
    'rochviewer.platform_profiles',
    'rochviewer.timings',
    'rochviewer.intel.intel_timings',
    # Imported lazily by intel_timings so the tables are built before a
    # sensor is read, which also hides these from the dependency scan.
    'rochviewer.intel.intel_board_sensors',
    'rochviewer.intel.intel_pch_smbus',
    'rochviewer.intel.intel_rapl',
    'rochviewer.memory.ddr4_tsod',
    'rochviewer.memory.ddr5_pmic',
    'rochviewer.memory.ddr5_spd',
    # Imported inside the Telemetry button handler, so the dependency scan
    # never sees either one.
    'rochviewer.memory.ddr5_telemetry',
    'rochviewer.ui.dimm_telemetry_window',
    'rochviewer.sensors.superio_lpc',
    # Imported inside the row getters that use them: the identity readings on
    # System Info, and the performance counters behind the Clocks section.
    'rochviewer.system_identity',
    'rochviewer.sensors.cpu_clocks',
    'rochviewer.unsupported_profile',
    'rochviewer.ui.display_values',
    'rochviewer.memory.dimm_inventory',
    'rochviewer.memory.dram_ic',
    'rochviewer.ui.lazy_read',
    'rochviewer.hardware.lowlevel_io',
    'rochviewer.hardware.pci_mcfg',
    'rochviewer.sensors.voltage_rails',
    # The card's own readings, its power limit and the error counter.
    #
    # Belt and braces, not a fix: PyInstaller scans bytecode and finds a
    # plain import inside a function body perfectly well. The entries here
    # cost nothing and say what the build is expected to carry, which
    # test_packaging then holds it to.
    'rochviewer.gpu.nvidia_gpu',
    # The AMD backend. Parts of it are reached by importlib with a name built
    # at runtime, which PyInstaller cannot follow, so those are named here by
    # hand -- test_packaging holds this list to that.
    'rochviewer.amd.profile',
    'rochviewer.amd.timings',
    'rochviewer.amd.power_metrics',
    'rochviewer.amd.apob',
    'rochviewer.amd.agesa',
    'rochviewer.amd.smn',
    'rochviewer.amd.smn_mcfg',
    'rochviewer.amd.fch_smbus',
    'rochviewer.amd.smu_clocks',
    'rochviewer.amd.smu_power',
    'rochviewer.amd.smu_voltages',
    'rochviewer.amd.adl',
    'rochviewer.amd.adlx',
    'rochviewer.gpu.radeon',
    'rochviewer.sensors.whea_errors',
]

a = Analysis(
    ['run_viewer.py'],
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
