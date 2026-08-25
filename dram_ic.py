# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""Conservative DRAM IC identification from module metadata.

Windows SMBIOS normally exposes the module part number, but not the actual DRAM
component marking.  This module therefore reports a die only when the supplied
metadata contains an explicit component/version code or matches a high-confidence
part-number rule.  Unknown modules stay Unknown IC instead of being guessed.
"""

from __future__ import annotations

import re
from typing import Iterable, Optional


def _clean(value: object) -> str:
    return re.sub(r"\s+", "", str(value or "").upper())


def _joined(values: Iterable[object]) -> str:
    return " ".join(str(value or "") for value in values).upper()


# Exact module part numbers that are useful when SMBIOS exposes the per-stick
# number rather than the retail kit SKU.
_EXACT_PART_NUMBERS = {
    # G.Skill Ripjaws V DDR4-3600 CL14 16 GB module (2x16 GB GVKA kit).
    "F4-3600C14-16GVKA": "Samsung B-die",

    # SK hynix 16 GB DDR5 OEM module used by the Z890 test system.
    # The user verified the installed HMCG78AGBUA081N modules are A-die.
    "HMCG78AGBUA081N": "SK hynix A-die",

    # TeamGroup 16 GB DDR5 module verified on the Z790 Apex test system.
    "TMXFL1680838KWK": "SK hynix A-die",

    # G.Skill 16 GB DDR5 modules confirmed SK hynix A-die on user kits.
    "F5-6000J2636G16G": "SK hynix A-die",
    "F5-6000J3040G16G": "SK hynix A-die",
    "F5-6000J3238G16G": "SK hynix A-die",
    "F5-6400J3239G16G": "SK hynix A-die",
    "F5-7200J3445G16G": "SK hynix A-die",
    "F5-7800J3646G16G": "SK hynix A-die",
    "F5-8000J3648G16G": "SK hynix A-die",
    "F5-8200J3852H16G": "SK hynix A-die",

    # Common Crucial Ballistix 8 GB Rev. E modules.
    "BL8G32C16U4B": "Micron E-die",
    "BL8G36C16U4B": "Micron E-die",
    "BLS8G4D30AESBK": "Micron E-die",
    "BLS8G4D32AESBK": "Micron E-die",
    "BLS16G4D30AESB": "Micron E-die",
    "BLS16G4D32AESB": "Micron E-die",
}


# Corsair label version codes.  These are only used if a version string is
# actually present in SMBIOS metadata (for example VER4.31 or V4.31).
_CORSAIR_VERSION_CODES = {
    "4.31": "Samsung B-die",
    "4.32": "Samsung C-die",
    "5.32": "SK hynix CJR",
    "5.33": "SK hynix DJR",
    "3.44": "Micron B-die",
    "8.31": "Nanya B-die",
}


def _decode_gskill_042_code(metadata: str) -> Optional[str]:
    """Decode a DDR4 G.Skill 042 code when firmware happens to expose it.

    Example compact form: 04213X8810B
      first character after X = IC density
      third character after X = IC manufacturer
      final character = die revision
    """
    compact = _clean(metadata)
    match = re.search(r"04213X[48S][0-9]([123459])[0-9]([A-Z])", compact)
    if not match:
        return None

    manufacturer_code, revision = match.groups()
    manufacturer = {
        "1": "Samsung",
        "2": "SK hynix",
        "3": "Micron",
        "4": "PSC",
        "5": "Nanya",
        "9": "JHICC",
    }.get(manufacturer_code)
    if not manufacturer:
        return None
    return f"{manufacturer} {revision}-die"


def identify_dram_ic(
    part_number: object,
    module_manufacturer: object = "",
    memory_type: object = "",
    capacity_gb: object = "",
    rank_label: object = "",
    extra_metadata: Iterable[object] = (),
) -> str:
    """Return a conservative user-facing IC label.

    The function deliberately prefers ``Unknown IC`` over a weak timing-bin
    guess.  It can be expanded later by adding exact part numbers or explicit
    manufacturer codes without changing the GUI.
    """
    part = _clean(part_number)
    maker = _clean(module_manufacturer)
    metadata = _joined((part_number, module_manufacturer, *extra_metadata))
    compact_metadata = _clean(metadata)

    if part in _EXACT_PART_NUMBERS:
        return _EXACT_PART_NUMBERS[part]

    # Explicit DRAM component markings are stronger than module-SKU guesses.
    component_rules = (
        (r"H5CG48AGBD", "SK hynix A-die"),
        (r"H5CG48MEBD", "SK hynix M-die"),
        (r"H5AN8G8NCJR", "SK hynix CJR"),
        (r"H5AN8G8NDJR", "SK hynix DJR"),
        (r"H5AN8G8NAFR", "SK hynix AFR"),
        (r"H5AN8G8NMFR", "SK hynix MFR"),
    )
    for pattern, label in component_rules:
        if re.search(pattern, compact_metadata):
            return label

    # Hynix OEM module part numbers often include the die family directly.
    hynix_tokens = {
        "CJR": "SK hynix CJR",
        "DJR": "SK hynix DJR",
        "AJR": "SK hynix AJR",
        "MJR": "SK hynix MJR",
        "AFR": "SK hynix AFR",
        "MFR": "SK hynix MFR",
    }
    if "HYNIX" in maker or part.startswith(("HMA", "HMC", "H5")):
        for token, label in hynix_tokens.items():
            if token in compact_metadata:
                return label

    # Decode G.Skill's explicit DDR4 042 production code when available.
    gskill_code = _decode_gskill_042_code(metadata)
    if gskill_code:
        return gskill_code

    # Decode Corsair's explicit label version only when it is present.
    for version, label in _CORSAIR_VERSION_CODES.items():
        if re.search(rf"(?:VER|VERSION|V)?\s*{re.escape(version)}(?:\D|$)", metadata):
            return label

    # High-confidence G.Skill DDR4 bins.  These rules are intentionally narrow.
    if part.startswith("F4-") or "GSKILL" in maker or "G.SKILL" in metadata:
        if re.match(r"^F4-(?:3200|3600|3800)C14-(?:8|16)G", part):
            return "Samsung B-die"
        if re.match(r"^F4-(?:4000C15|4133C17|4266C17|4400C17)-(?:8|16)G", part):
            return "Samsung B-die"

    # Crucial/Micron DDR4 Rev. E identifiers commonly present in part numbers.
    if "AES" in part:
        return "Micron E-die"
    if re.match(r"^BL8G(?:30C15|32C16|36C16)U4B", part):
        return "Micron E-die"

    # Some OEM DIMMs expose an explicit die family token in the module number.
    explicit_die = re.search(
        r"(?:SAMSUNG|MICRON|HYNIX|SKHYNIX)[-_ ]*([A-Z])[-_ ]?DIE",
        metadata,
    )
    if explicit_die:
        vendor = "SK hynix" if "HYNIX" in explicit_die.group(0) else explicit_die.group(0).split()[0].title()
        return f"{vendor} {explicit_die.group(1)}-die"

    # Identifying only the DRAM maker is still useful for OEM modules, but make
    # it clear that the die revision was not exposed.
    if part.startswith("MTA") or "MICRON" in maker:
        return "Micron (die unknown)"
    if part.startswith("M378") or "SAMSUNG" in maker:
        return "Samsung (die unknown)"
    if part.startswith(("HMA", "HMC")) or "HYNIX" in maker:
        return "SK hynix (die unknown)"

    return "Unknown IC"
