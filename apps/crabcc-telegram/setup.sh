#!/usr/bin/env bash

# --- Logger Setup ---
LOG_FILE="telegram_setup.log"
exec > >(tee -i "$LOG_FILE") 2>&1

set -e 

# --- Formatting ---
GREEN='\033[0;32m'
BLUE='\033[0;34m'
YELLOW='\033[1;33m'
NC='\033[0m'

echo -e "${BLUE}===> Starting Crabcc Telegram Bot Setup <===${NC}"

# 0. Navigate and Check Environment
TARGET_DIR="$HOME/workspace/bin/crabcc/apps/crabcc-telegram"

if [ ! -d "$TARGET_DIR" ]; then
    echo -e "${RED}✗ Directory not found: $TARGET_DIR${NC}"
    exit 1
fi

cd "$TARGET_DIR"

if ! command -v cargo &> /dev/null; then
    echo -e "${YELLOW}! Cargo (Rust) not found. Please install via rustup.rs first.${NC}"
    exit 1
fi

# 1. Set up the .env
echo -ne "${YELLOW}Enter your TELEGRAM_BOT_TOKEN (from BotFather): ${NC}"
read -r BOT_TOKEN

if [ -z "$BOT_TOKEN" ]; then
    echo -e "${RED}! Token cannot be empty.${NC}"
    exit 1
fi

echo -e "\n${BLUE}[1/4] Creating .env with secret-hygiene...${NC}"
cat > .env <<EOF
TELEGRAM_BOT_TOKEN=$BOT_TOKEN
# Optional — for /dashboard Mini App via ngrok / cloudflared:
# CRABCC_PUBLIC_URL=https://your-tunnel.ngrok.io
EOF

chmod 0600 .env
echo -e "${GREEN}✓ .env created and locked down (0600).${NC}"

# 2. Build and Install Binary
echo -e "\n${BLUE}[2/4] Building standalone binary (Release)...${NC}"
cargo build --release

echo -e "Copying binary to ~/.cargo/bin/..."
mkdir -p ~/.cargo/bin
cp target/release/crabcc-telegram ~/.cargo/bin/

# 3. Install the LaunchAgent
echo -e "\n${BLUE}[3/4] Registering macOS Service...${NC}"
# Ensure ~/.cargo/bin is in PATH for this session
export PATH="$HOME/.cargo/bin:$PATH"
crabcc-telegram install-service

# 4. Verify
echo -e "\n${BLUE}[4/4] Verifying LaunchAgent status...${NC}"
echo "------------------------------------------------"
SERVICE_NAME="gui/$(id -u)/com.crabcc.telegram-bot"
launchctl print "$SERVICE_NAME" | head -n 10

echo -e "\n${GREEN}Setup complete!${NC}"
echo -e "${YELLOW}Streaming logs now (Ctrl+C to stop):${NC}"
echo "------------------------------------------------"
tail -f ~/Library/Logs/Crabcc/telegram-bot.{out,err}.log
