# AMD Misc decoding notes

The implementation reads the existing native UMC/APOB transports. It does not
load the reference tool, scrape its window, or require HWiNFO. Reference screenshots
are comparisons, not runtime defaults. No memory tuning writes were added.

## Postamble

The supplied reference executable 1.0.0 has SHA-256
`04abb5a04113acd3d75f50bcf6f1f508dc2dee211a5740a902a52c2741bb15a4`.
Static inspection found UMC 0x502A4 read/write postamble descriptors at RVAs
0x94A428 / 0x94A478, connected to formatter tables at 0x950AD0 / 0x950B10.
Their formatters (RVAs 0x59AC0 / 0x59B20) distinguish codes 0 and 1 and reject
other codes: 0 is 0.5 tCK (pattern 0); 1 is 1.5 tCK (read 010, write 000).
The native three-bit fields remain those documented by ZenStates-Core's
DDR5Dictionary.cs. Reserved values are not extrapolated.

## Raw training compatibility labels

the reference tool's descriptors at RVAs 0x94AB58 through 0x94AEA0 use the same synthetic
APOB byte-address namespace as its RTT/ODT/drive descriptors. The byte offsets
below are read from Roch's already validated per-channel Granite Ridge records:

| Reference label | Record byte |
|---|---|
| ALERT_PU | 0x10 |
| CA_DRV | 0x10 |
| PHY_VREF | 0x11 |
| DQ_VREF | 0x12 |
| CA_VREF | 0x13 |
| CS_VREF | 0x14 |
| RX_DFE | 0x15 |
| TX_DFE | 0x16 |

These names are **compatibility labels, not validated physical meanings**.
Notably 0x11..0x13 already decode as CA/CK/CS drive strengths. ALERT_PU and
CA_DRV alias the same byte in the reference. Therefore the UI explicitly says
“Raw training codes” and supplies no voltage or percent unit.
The known drive-strength decoders remain unchanged. Unsupported CPUs, missing
channels and rejected records remain unavailable. CS_VREF is listed once.

ECC is the UMC DimmEccEn bit (0x50100 bit 12), not DDR5 on-die ECC. The short
UI label is ECC; this document and README retain that distinction.
