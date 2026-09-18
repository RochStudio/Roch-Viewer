# LGA1700 DDR4 RON: reading and validation

This 1.0.4 test build starts from upstream main commit
`7dbc6789a5e8e754d0a2df8b7369a1e7dc25941f` (fetched 2026-09-18).

## What changed

- Training and Summary show one **DRAM RON** row instead of Pull Up Drv and Pull Down Drv. Both old DDR4 rows called the same function; they did not represent independently measured CPU pull-up/down impedance.
- DDR4 DQ/DQS ODT NOM/WR/PARK rows are removed. They duplicated the existing RTT NOM/WR/PARK functions. RTT and IMC Dq Odt Vref controls remain.
- LGA1700 DDR5 and LGA1851 keep their two driver rows and six CA/CS/CK ODT rows. AMD is unchanged.
- The RON reader reads each complete 32-bit source before decoding. Empty/all-ones/failed reads cannot produce a plausible 34-ohm default. If sources disagree, or just one is unavailable, both results are shown as W0/W1. Their physical rank/slot mapping is not established, so they are not labeled R0/R1.

## What the value means

The nominal DDR4 output-driver selection is MR1 bits A2:A1:

| A2:A1 | Nominal RON |
|---|---|
| 00 | 34 ohms, RZQ/7 |
| 01 | 48 ohms, RZQ/5 |
| 10 / 11 | Reserved |

Source: manufacturer-authored [ISSI IS43/46QR81024A / IS43/46QR16512A datasheet](https://issi.com.cn/WW/pdf/43-46QR81024A-16512A.pdf), Mode Register 1, page 19. These are mode-register encodings, not ASUS Setup's Auto=0, 34=1, 48=2 enumeration.

The viewer uses the existing inferred controller-shadow layout: MR1 is the upper halfword of the dword at each channel base plus E5A0 or F5A0. DDR4 bases are FEDC0000 and FEDD0000. Thus `(dword >> 17) & 3` supplies the RON code. No new register address or inverted encoding has been invented to force agreement with the BIOS selection.

The MR1 encoding is specification-backed. The user has now confirmed that the viewer follows both BIOS RON choices on this ASUS board with DIMM RON Training and MRC Fast Boot disabled. This provides board-specific behavioral validation of the readout. The exact per-window rank/slot mapping remains unverified; the software reads controller copies, not physical resistance and not a direct DRAM mode-register read transaction.

## Current hardware evidence

On 2026-09-18 the user confirmed the working configuration on the ASUS ROG STRIX Z790-A GAMING WIFI D4, BIOS 3202 / microcode 0x11F:

- MRC Fast Boot: Disabled.
- DIMM RON Training*: Disabled.
- DRAM Mr1 RON 34 ohms: viewer reports 34 ohms.
- DRAM Mr1 RON 48 ohms: viewer changes to 48 ohms.

This supersedes the earlier unchanged-48 result below. Both settings were disabled for the successful test; their individual contribution has not been isolated. Evidence for success is the user's report, with no new raw dumps supplied for these successful runs. This validates the setting/readout correspondence on this configuration, not electrical resistance or stability, and is not a claim for all LGA1700 boards.

Decoder tests cover 34, 48, both reserved codes, neighboring bits, independent channels, changing readings, empty/unmapped/failed reads, and differing windows. Those tests establish software behavior for supplied raw words, not physical hardware correctness.

## Compare the two boots

### Received comparison, 2026-09-18

The user supplied `ron 34 in bios.txt` and `ron 48 in bios.txt`, each with 265 unique rows. In both files, both channel columns at both E5A0 and F5A0 report packed MR0/MR1 `0x00030D70`, MR1 `0x0003`, and ODI `01`. DRAM RON reads `48 Ohm | 48 Ohm`. Thus the sampled raw source did not change; the decoder's 48-ohm output follows those bits. This does not independently validate that the shadow is the final DRAM RON state.

Only three rows differ: Core Ratio (52.9 vs 52.8), DLL_CODEPI (42 vs 43), and Manufactured (unavailable vs 10/2022), in 34/48 order. These differences do not establish a RON change or prove full memory retraining occurred.

The exact BIOS IFR contains MRC Fast Boot under Ai Tweaker > DRAM Timing Control (SaSetup 0x191), and DIMM RON Training* under its Memory Training Algorithms submenu (OCMR 0x23F). Their saved values were not in these viewer dumps. The suggested follow-up tested with both disabled; the user subsequently confirmed successful 34/48 readout changes as recorded above. Do not silently substitute the BIOS-requested value for the raw shadow reading.

Input SHA256:
- 34: `a2fb404eaa9d7aad6351e5b4f2ecf7b2e1dc3acc24c2682f82259010bb075f51`
- 48: `458efd76f2151019ae0856c3d45dcd0a507ba46221903d708c2b3d91d580077c`

### Original capture procedure

1. Keep this build's `RochViewer.exe` beside your existing `inpoutx64.dll`. The driver is not included in this package. Run as administrator on the ASUS DDR4 PC.
2. With 34 ohms selected in BIOS, save the setting and boot Windows. Reopen BIOS if needed to verify the selection persisted. Record whether memory fast boot/training reuse was enabled; reused training is a possibility to investigate, not a diagnosed cause.
3. Open **Advanced**, search **MR1**, then click **Dump**. The existing dump button writes `RochViewer.txt` to the Windows Desktop. Rename it to `RON-34.txt` before another dump overwrites it.
4. Repeat with 48 ohms and save `RON-48.txt`. Use the same other BIOS settings and note any difference in boot/training behavior.
5. Return both text dumps for comparison. No further firmware flash is required to run this viewer test.

The added diagnostics are paired by channel and include `DDR4 MR0/MR1 @E5A0`, `DDR4 MR1 ODI @E5A0`, and the corresponding F5A0 fields. The full dword, extracted MR1 and ODI code show exactly what was decoded.

If ODI remains 01 in both boots, this read path still reports 48; the viewer must not substitute the requested 34. Then investigate whether ASUS applies the setting, whether training replaces it, and whether a different final MR1 source is required. If the raw code changes 00 to 01, the expected displayed values are 34 to 48. This is not a claim of measured resistance.
