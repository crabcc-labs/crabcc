# CrabCC Design Specification

## Visual Identity: Liquid Glass Synth
The CrabCC interface is a high-density "modular synth" dashboard designed for developer focus. It balances technical precision with a "liquid-glass" aesthetic.

### Core Principles
- **No-Card Architecture**: Hierarchy is established through grid lines, technical borders, and background depth rather than standard container cards.
- **Materiality**: UI panels use 12px-16px backdrop blur with 70-80% opacity to create a layered, frosted glass effect.
- **Light Leaks**: Ultra-thin (0.5px - 1px) internal borders with low-opacity white/blue simulate light catching on glass edges.
- **Typography**: Strictly monospaced or high-legibility sans-serifs (Inter/Space Grotesk) to maintain a terminal-like feel.

### Color Palette
- **Surface**: #0A0A0A (Deep charcoal/black)
- **Glass**: rgba(10, 10, 10, 0.75) with backdrop-filter
- **Primary**: #007AFF (Electric Blue)
- **Secondary/Status**: #39FF14 (Radioactive Green)
- **Border**: #1F1F1F (Subtle grid lines)

### Iconography
- **Technical/Line Art**: Minimalist stroke icons (1.5px weight).
- **Pixel-Art Accents**: Lo-fi 8-bit crab sprites for status indicators and loading states to contrast the high-fidelity glass.

---

## Component Standards
- **Top Nav**: Includes a breadcrumb-style context indicator (e.g., `CRAB_CC / CORE_ENGINE`) in a frosted pill container.
- **Side Nav**: Compact, icon-heavy with vertical text labels or small-caps monospace.
- **Timeline**: Real-time event stream with millisecond precision and success/error status pips.
- **Command Launchpad**: Centrally located input for quick-action terminal commands.
