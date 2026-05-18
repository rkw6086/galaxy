#!/bin/bash
# Resets all Galaxy app state to a fresh install.
# Run this before launching galaxy-oss if you want a clean slate.

set -e

echo "Resetting Galaxy to fresh state..."

# Home config/data
rm -rf ~/.galaxy

# macOS Application Support (session history, chat history, workspace state)
rm -rf ~/Library/Application\ Support/com.samsung.Galaxy
rm -rf ~/Library/Application\ Support/com.samsung.Galaxy-Stable
rm -rf ~/Library/Application\ Support/com.samsung.GalaxyAI
rm -rf ~/Library/Application\ Support/samsung.galaxy.GalaxyOss
rm -rf ~/Library/Application\ Support/samsung.galaxy.GalaxyStable
rm -rf ~/Library/Application\ Support/samsung.galaxy.GalaxyDev
rm -rf ~/Library/Application\ Support/samsung.galaxy.GalaxyLocal
rm -rf ~/Library/Application\ Support/samsung.galaxy.GalaxyPreview

# macOS Saved Application State (window restoration)
rm -rf ~/Library/Saved\ Application\ State/com.samsung.Galaxy.savedState
rm -rf ~/Library/Saved\ Application\ State/com.samsung.Galaxy-Stable.savedState
rm -rf ~/Library/Saved\ Application\ State/com.samsung.GalaxyAI.savedState
rm -rf ~/Library/Saved\ Application\ State/samsung.galaxy.GalaxyOss.savedState
rm -rf ~/Library/Saved\ Application\ State/samsung.galaxy.GalaxyStable.savedState
rm -rf ~/Library/Saved\ Application\ State/samsung.galaxy.GalaxyDev.savedState
rm -rf ~/Library/Saved\ Application\ State/samsung.galaxy.GalaxyLocal.savedState

# macOS Caches
rm -rf ~/Library/Caches/com.samsung.Galaxy
rm -rf ~/Library/Caches/com.samsung.Galaxy-Stable
rm -rf ~/Library/Caches/com.samsung.GalaxyAI
rm -rf ~/Library/Caches/samsung.galaxy.GalaxyOss
rm -rf ~/Library/Caches/samsung.galaxy.GalaxyStable
rm -rf ~/Library/Caches/samsung.galaxy.GalaxyDev
rm -rf ~/Library/Caches/samsung.galaxy.GalaxyLocal

# Logs
rm -f ~/Library/Logs/galaxy*.log*

# macOS Preferences
defaults delete com.samsung.Galaxy 2>/dev/null || true
defaults delete com.samsung.Galaxy-Stable 2>/dev/null || true
defaults delete com.samsung.GalaxyAI 2>/dev/null || true
defaults delete samsung.galaxy.GalaxyOss 2>/dev/null || true
defaults delete samsung.galaxy.GalaxyStable 2>/dev/null || true
defaults delete samsung.galaxy.GalaxyDev 2>/dev/null || true
defaults delete samsung.galaxy.GalaxyLocal 2>/dev/null || true

echo "Done. Galaxy will start fresh on next launch."
