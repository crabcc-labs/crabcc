# Knowledge Graph: Design & Animation Spec

## Overview
The Knowledge Graph View is a specialized interface for visualizing complex data relationships. It utilizes a three-dimensional topology map rendered with glassmorphism and light-based effects.

## Visual Design
- **Topology Map**: Nodes are represented as glowing points of light connected by "neural pathways" (thin, low-opacity lines).
- **Node Inspection**: A side panel that slides in from the right, using 20px backdrop blur to float over the graph without obscuring it.
- **Temporal Flow**: A timeline at the bottom that uses glass blocks to represent data density over time.

## Interaction & Animation Logic
### 1. The "Pulse" (Ambient)
- **Logic**: Nodes should have a subtle, asynchronous breathing animation (opacity shift from 60% to 100%).
- **Duration**: 4000ms, Ease-in-out.

### 2. Neural Pathway Ingest
- **Logic**: When a new memory stream is ingested, a "light packet" (a 2px dot) travels from the ingest point to the central node, followed by a ripple effect.
- **Visual**: `box-shadow: 0 0 10px #007AFF;` on the packet.

### 3. Node Selection
- **Logic**: On click, the selected node scales by 1.2x and its connections intensify in brightness. The background graph slightly desaturates to focus on the active cluster.

### 4. Liquid Glass Transitions
- **Logic**: Panels appearing over the graph should use a "refraction" fade.
- **CSS**: `backdrop-filter: blur(0px)` to `blur(16px)` over 300ms.
