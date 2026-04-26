### TODO

#### 1. Merge HAP into Existing Server (`:5002`)

- Remove dedicated HAP server (`:5010`)
- Run HAP within the same runtime and listener as the main server
- Ensure HAP has access to shared `ctx` (no duplication of state or logic)
- Verify no additional listeners/threads are spawned

#### 2. Extend Existing Command Layer (Single Source of Truth)

- Remove the duplicated logic (e.g. `execute_blind_command` and `process_command`)
- Avoid internal HTTP/WebSocket calls — use direct function calls

#### 3. Validate Accessory Initialization Behavior

- Confirm that merging servers removes unintended `UP` trigger on registration
- If still present:
  - Prevent command execution during HAP accessory setup/sync phase
- Ensure initialization is **read-only** and does not cause side effects

#### 4. Persist Last Known State (Minimal Scope)

- Store last known device state (for HAP consistency on restart)
- Reload state on server start and expose it to HAP
- Do **not** use this state to actively control/reset devices

#### 5. Verify Server Lifecycle & Cleanup

- Confirm that merging into `:5002` resolves shutdown issues
- Ensure:
  - Clean shutdown (no non-zero exit)
  - Proper task termination
  - GPIO/resources released correctly
- Remove any cleanup logic that was only needed for the separate HAP server

#### 6. Routing & Structure (Non-invasive Refactor)

- Integrate HAP handlers into existing router (e.g. Axum)
- Keep HAP as an extension of the current system, not a separate module
- Maintain clear boundaries without over-abstracting

---

### Verification Checklist

- [ ] Only one server running on `:5002`
- [ ] No duplicated command paths or logic
- [ ] REST / WebSocket / HAP all use the same command layer
- [ ] No commands triggered during accessory registration
- [ ] HAP reflects correct state after server restart
- [ ] Server exits cleanly without errors
