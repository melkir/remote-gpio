### TODO

#### 1. Extend Existing Command Layer (Single Source of Truth)

- Remove the duplicated logic (e.g. `execute_blind_command` and `process_command`)
- Keep HAP on its dedicated listener, but share the same in-process command API
- Avoid internal HTTP/WebSocket calls between subsystems — use direct function calls

#### 2. Validate Accessory Initialization Behavior

- Confirm whether initial HomeKit synchronization triggers an unintended `UP`
- If still present:
  - Prevent command execution during HAP accessory setup/sync phase
- Ensure initialization is **read-only** and does not cause side effects

#### 3. Persist Last Known State (Minimal Scope)

- Store last known device state (for HAP consistency on restart)
- Reload state on server start and expose it to HAP
- Do **not** use this state to actively control/reset devices

#### 4. Verify Server Lifecycle & Cleanup

- Ensure:
  - Clean shutdown (no non-zero exit)
  - Proper task termination
  - GPIO/resources released correctly
- Confirm the HAP listener and mDNS announcement both unwind cleanly
- Remove any cleanup logic that no longer applies after HAP stabilization

#### 5. HAP Structure (Non-invasive Refactor)

- Keep HAP as part of the same process, not a separate service
- Keep transport/protocol code isolated from the shared command layer
- Maintain clear boundaries without over-abstracting

---

### Verification Checklist

- [ ] No duplicated command paths or logic
- [ ] REST / WebSocket / HAP all use the same command layer
- [ ] No commands triggered during accessory registration
- [ ] HAP reflects correct state after server restart
- [ ] Server exits cleanly without errors
