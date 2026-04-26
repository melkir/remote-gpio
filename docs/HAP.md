# 🦀 Minimal Rust HAP Server (WindowCovering)

## 🎯 Goal

Build a tiny HomeKit-compatible HAP server in Rust for a single WindowCovering accessory, while keeping Homebridge as fallback.

---

## 🧩 Scope (strict)

- 1 accessory
- 1 service: WindowCovering
- 3 characteristics:
  - CurrentPosition
  - TargetPosition
  - PositionState
- No plugins, no abstractions

---

## 🏗️ Core Components

### 1. mDNS (discovery)

Advertise \_hap.\_tcp with:

- id=XX:XX:XX:XX:XX:XX
- md=MyBlinds
- sf=1 (unpaired) / 0 (paired)
- ci=203 (WindowCovering)

👉 Use Avahi

---

### 2. HTTP Server

Use axum or hyper

#### GET /accessories

Return static JSON:
json { "accessories": [{ "aid": 1, "services": [{ "type": "WindowCovering", "characteristics": [ { "iid": 1, "type": "CurrentPosition", "value": 50 }, { "iid": 2, "type": "TargetPosition", "value": 50 } ] }] }] }

---

#### GET /characteristics?id=1.1

Return current state

#### PUT /characteristics

Update target position → call your Rust logic

---

### 3. Pairing (hard part)

Implement:

- SRP
- Ed25519
- ChaCha20-Poly1305

👉 Use HAP-NodeJS as behavioral reference

---

### 4. Device Logic (your code)

rust fn set_target_position(v: u8); fn get_current_position() -> u8;

---

## 🚀 Phases

1. Discovery
   - mDNS visible in Home app

2. Static accessory
   - /accessories works

3. Pairing
   - Device can be added

4. Control
   - /characteristics wired to blinds

---

## ⚠️ Notes

- iOS is strict → match responses exactly
- Keep everything hardcoded
- Debug against HAP-NodeJS behavior

---

## ✅ Success Criteria

- Device appears in Home app
- Pairing succeeds
- Blinds respond to slider

---

## 🧠 Mindset

> Build the smallest thing that HomeKit accepts, not a framework
