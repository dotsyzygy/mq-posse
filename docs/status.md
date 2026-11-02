# Status

Phase 1 is finished and has run on two clients in game. Build order is the numbered phases under **Features**, and usage is in the [README](../README.md).

`posse-core` (the block, slots, status and roster) and `posse-shm` (the Windows side) test without the game: `cargo test --workspace --exclude mq2-posse`. The plugin itself (`src/`) can only be checked in game.

## Features

### 1. Presence and core status

- [x] Shared memory block per server and posse name, with a layout version that mismatched builds refuse to join
- [x] Claiming and releasing a slot, with up to 64 clients
- [x] Joining automatically or by `/posse join`, and `/posse leave`
- [x] Publishing core status each pulse, with torn reads prevented by a sequence counter
- [x] Member states (`InGame`, `Zoning`) from game state and zone callbacks
- [x] Crash and exit detection by holding a handle to each peer's process, checked every 100 ms
- [x] `.Age` and `.Stale`, measured on the clock all clients share
- [x] `Posse` TLO: `Count`, `Members`, `Member[x]`, `[x]`
- [x] Member type: `GameState`, `Zone`, `TargetID`, `Age`, `Stale`, `Spawn`, and the always-ready `Me` members listed in the README, as plain values for now (`Class.ShortName` and the like need phase 3)
- [x] A clear chat error when the shared memory can't be opened
- [x] `/posse list` and `/posse stats`
- [x] `MQ2Posse.ini` settings `Posse` and `AutoJoin`
- [x] Run on two real clients, joining automatically and reading each other's status
- [x] Compare with the owner's `${Me}`: HP, mana, heading, `CountBuffs`, `Buff[n]` names and durations
- [x] A client's process killed in game drops out of `/posse list` straight away
- [x] Compare the rest with the owner's `${Me}`: endurance, class, race, `Combat`, `Sitting`, `Moving`, `Invis`, `Casting`, `Song`, `Pet` and position. `Stunned`, `Feigning` and `Hovering` share their code but haven't been seen working
- [x] Leaving the posse at character select, and joining again on entering the game
- [x] Staying listed as `Zoning` through a zone change
- [x] Measure per-pulse cost with `/posse stats`: 27 to 30 us average and 563 us worst over 9,284 pulses, with two clients
- [x] Measure status latency, including a client at a low frame rate: 16 ms average and 34 ms worst at its normal rate, 48 ms and 100 ms at 10 fps

### 2. Commands

- [x] `/pca`, `/pcaa`, `/pgc`, `/pgca`, `/prc`, `/prca`, `/pzc` and `/pzca`, tested outside the game (named `/pc` and `/pca` until `/pc` became the command to a name)
- [x] All eight run in game on three clients, in a group and a raid
- [x] Measure how long commands take to arrive: 2 to 103 ms over 7 samples, from the debug log
- [x] `/pzc` and `/pzca` in game, including instances
- [x] `AllowCommands=0` refusing a command in game, before it became `true` or `false`
- [ ] Seen in game as they come up, since they can't be set up on purpose: a command dropped after 5 seconds, `.TimedOut` and `.Dropped` in `${Posse.LastCommand}`, and lost commands. Zoning takes under 5 seconds, so a load screen doesn't cause them. Tests outside the game cover all of these
- [x] `/posse help`, with a test that every command in it is in the README
- [x] `AllowCommands` setting in `MQ2Posse.ini`
- [x] `/posse allowcommands true` and `false`, saved like `Debug` through a reload, and `AllowCommands=false` refusing a command in game
- [x] `/posse debug on` and `off`, showing what's sent and received in game, and `Debug` in `MQ2Posse.ini` keeping it on through a reload
- [x] Dropping a command after 5 seconds, and saying so on the client that dropped it
- [x] A client saying so when commands were overwritten before it read them
- [x] `/pc` to a name, tested outside the game
- [x] Completion tracking in `${Posse.LastCommand}`, which also tells the sender about dropped and lost commands, tested outside the game
- [x] `.Spawn` NULL for a member in another instance of your zone, checked in game
- [x] Named channels of clients, like DanNet's channels: creating, joining, leaving, deleting and listing them, saving them in `MQ2Posse.ini`, and `/pnc` and `/pnca` to send to one, tested outside the game
- [x] All of the last three run in game on three clients, including `.Pending` while a target zoned and channels coming back after every client restarted

### 3. Generic `Me` passthrough

Built on mq-emu's `DataTypeDef::any_member`.

- [x] Any `Me` path on a member, returning NULL until its first value arrives, including `State`, which is `${Me.State}` and not the member's `GameState`
- [x] A generated, committed list of MacroQuest's member names (1,135, extracted from `ScopedTypeMember` in its `datatypes/*.cpp`) for `any_member`, since Lua can read only listed names: mq-emu 0.1.4's `MACROQUEST_MEMBERS`, with `MACROQUEST_METHODS` for refusing methods
- [x] Returning real ints, floats and bools once a value is cached
- [x] Requests and results in shared memory, each in the slot of the client writing it (a reader's requests in its own, an owner's values in its own), with path validation (relative to `Me`, no `${`, no methods) on both sides
- [x] Per-pulse evaluation budget, with each path re-evaluated no more often than every `max(10 x its evaluation cost, 100 ms)`
- [x] Reading a path starts evaluating it, with no separate command for that
- [x] Paths unread for 30 seconds stop being evaluated but keep their last value, and the next read returns that value and starts evaluating again
- [x] A way for a macro to see how old a path's value is, since `.Age` only covers the member's status: `.PathAge[path]`
- [x] The same on `.Pet` and `.Buff`/`.Song` values, for their members MQ2Posse doesn't publish
- [x] All of the above checked in game on two clients, including Lua reading typed values and a method being refused

### 4. Hardening

- [x] Unit tests of slots, sequence counter and roster in a separate pure Rust crate, since the plugin crate can't run tests outside the game; failure cases are permanent negative tests
- [x] Tests of the shared memory between real processes: killing a client, reusing its slot, and mappings from another build
- [x] The same for commands
- [x] The same for path validation, with phase 3
- [ ] In-game checks with several clients: reloading the plugin on one client, a burst of commands, a mismatched build
- [x] A check with one client limited to a low frame rate

## Missing

### Needed from mq-emu

Nothing. Tag `v0.1.1`, which Posse uses, added the two things it was missing: the catch-all member (`DataTypeDef::any_member`), and class, race and endurance getters (`Spawn::class`, `Spawn::race`, `Mq::class_name`, `Mq::race_name`, `Mq::endurance_current`, `Mq::endurance_max`).

### Needed from elsewhere

Nothing. `windows-sys` provides the file mapping and process handles, in `posse-shm`: shared memory belongs in the plugin, not in the bindings.

## Out of scope

- **LAN or internet support.** Posse works on one PC only, and any change to this would need a second transport, such as MacroQuest's actors.
- Clients on different servers, or more than one posse per client.
- Clients under different Windows users, at different elevation, or in a sandbox.
- Protection from other programs running as your Windows user.
- Lua messaging through `mq.actors`. Lua reads the `Posse` TLO and uses `/posse` commands instead.
- Calling methods through a remote `Me`. Use `/pc`.
- Reading another plugin's added `Me` members from Lua, since they aren't on the generated list. Macros can read them.
- Waiting for a value within a read: a read can't block, so an unread path returns NULL at first.
- Running a mix of plugin builds, including upgrading one client at a time.
