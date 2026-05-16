# CrabCC Visual Assets & Iconography Spec

## Overview
This document specifies the design principles and technical standards for the brand identity and secondary iconography of CrabCC. It bridges the gap between high-fidelity "liquid glass" UI and lo-fi "pixel-art" functional indicators.

## 1. Brand Identity: The Core Mark
The CrabCC logo is a geometric synthesis of organic forms and technical precision.

### Visual Logic
- **Structure**: A stylized "C" (for Crab/Core) formed by circuit-path line work.
- **Silhouette**: Integrated crab pincer motifs within the hexagonal/circuit frame.
- **Styling**: 
    - **Stroke Weight**: 2px - 3px for high visibility at small scales.
    - **Corners**: Sharp, technical 45-degree angles to match the terminal aesthetic.
    - **Color**: `#007AFF` (Electric Blue) on a `#000000` (True Black) ground.
- **Typography**: Square, wide-set sans-serif with technical "cut-outs" in the letterforms to mirror the icon's geometry.

## 2. Iconography: Pixel-Art System
Used for status indicators, loading sequences, and "boot" alerts to provide a human, lo-fi contrast to the high-density glass UI.

### Technical Standards
- **Grid**: 32x32 or 64x64 pixel canvas.
- **Palette**: 
    - **Primary**: `#007AFF` (Crab Body)
    - **Status**: `#39FF14` (Glowing Eyes/Indicators)
    - **Accent**: Pure white for high-contrast pips.
- **Rendering**: Nearest-neighbor scaling to maintain crisp edges. No anti-aliasing.

### Animation States (The "Loading Crab")
1. **The Pulse**: Eyes flicker between `#39FF14` and `#1A660A` every 500ms.
2. **The Ingest**: Claws snap rhythmically as data packets (pixel dots) move from left to right.
3. **The Complete**: Crab sprite scales up slightly with a green "glow" box-shadow.

## 3. Usage Guidelines
- **High-Fidelity Contexts**: Use the geometric vector logo for top-nav branding and splash screens.
- **Low-Fidelity Contexts**: Use the pixel-art sprites for terminal output, status badges, and real-time processing indicators.
- **Contrast Rule**: Never place pixel art inside glass panels with high transparency; use them against solid dark surfaces to ensure the "8-bit" aesthetic pops.
