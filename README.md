# MQ2Posse

A MacroQuest plugin that lets EverQuest clients on the same PC see each other's status and send
each other messages and commands. It adds a `Posse` TLO to macros and Lua.

> **Status: early.** Joining a posse, status, the `Posse` TLO and `/posse list`, `join`, `leave` and
> `stats` are built and have run on two clients. Messages, commands between clients and reading
> `Me` members outside the always-ready list aren't yet, and this README describes them as designed.
> See [docs/status.md](docs/status.md) for what exists and what's missing.

It's built on [mq-emu](https://github.com/dotsyzygy/mq-emu-rs), so it runs only on emulator servers
with the RoF2 client (MacroQuest's `emu` branch, 32-bit).

**One PC only.** Posse doesn't work over a LAN or the internet. It adds no network traffic and opens
no ports.

## What it does

- **Finds your other clients:** every client on the PC with the same server and posse name joins
  automatically.
- **Shows each client's status:** read another character's HP, mana, buffs, position and more from
  any client, as that character's own client reports them.
- **Reads almost anything else:** ask for any `Me` member not in the built-in set, and the owning
  client evaluates it and keeps it updated.
- **Sends messages and commands:** to one character, your group or the whole posse.
- **Says how fresh the data is:** every member reports how old its data is and whether it's gone
  stale.

Unlike `${Spawn[Bob]}` or `${Group}`, this data comes from Bob's own client, so it works across zones
and includes things other clients can't see, such as Bob's exact mana and full buff list.

## Install

You need Rust with the `i686-pc-windows-msvc` target, `MQ_ROOT` pointing at your MacroQuest checkout,
and `cargo-mq`:

```
cargo install --git https://github.com/dotsyzygy/mq-emu-rs cargo-mq
cargo mq doctor
cargo mq deploy
```

Then load it on **every** client that should take part:

```
/plugin MQ2Posse
```

All clients must run the same build of the plugin. A client with a different build refuses to join
and says why, so to upgrade, unload the plugin everywhere first.

All clients must also be able to open the same shared memory, so run them under the same Windows
user, at the same elevation and outside any sandbox. A client that can't join says so in chat.

## Commands

| Command | What it does |
|---|---|
| `/posse` or `/posse list` | Lists members with their state and data age |
| `/posse join <name>` | Leaves the current posse and joins `<name>`, until you next go to character select |
| `/posse leave` | Leaves the posse until the next `/posse join` |
| `/posse tell <target> <text>` | Puts a message in the target's inbox |
| `/posse cmd <target> <command>` | Runs a command on the target |
| `/posse next` | Removes the oldest message from your inbox |
| `/posse observe <name> <path>` | Starts keeping a `Me` path from that member up to date |
| `/posse unobserve <name> <path>` | Stops keeping it up to date |
| `/posse stats` | Shows what publishing status costs this client per pulse, and how old each member's status has been |
| `/posse stats reset` | Starts those numbers again |

`<target>` is one of:

| Target | Who it reaches |
|---|---|
| A character name | That character |
| `group` | Posse members in your group, not you |
| `others` | Every other member |
| `all` | Every member, including you |

For example:

```
/posse cmd others /stand
/posse cmd Bob /target id ${Me.ID}
```

As usual in MacroQuest, `${...}` in a command is expanded on the client that sends it. Here,
`${Me.ID}` is the sender's ID.

## The Posse TLO

| Member | Type | Meaning |
|---|---|---|
| `${Posse}` | string | The posse's name, or NULL if not in one |
| `${Posse.Count}` | int | Members currently online, including you |
| `${Posse.Members}` | string | Their names, separated by spaces |
| `${Posse.Member[Bob]}` or `${Posse[Bob]}` | member | That member, or NULL if not online |
| `${Posse.Inbox}` | inbox | Number of messages waiting |
| `${Posse.Inbox.From}` | string | Sender of the oldest message |
| `${Posse.Inbox.Text}` | string | Text of the oldest message |
| `${Posse.LastCommand.Pending}` | int | Targets of your last `/posse cmd` that haven't run it yet |
| `${Posse.LastCommand.Done}` | int | Targets that have run it |
| `${Posse.LastCommand.TimedOut}` | int | Targets that didn't run it within 5 seconds |

`Done` means the target's client ran the command, not that it worked in game: `/cast` can still
fizzle.

A member has `${Me}`'s members, under the same names, so `${Posse[Bob].PctHPs}` is Bob's
`${Me.PctHPs}`. It also has these, which `${Me}` doesn't:

| Member | Type | Meaning |
|---|---|---|
| `.GameState` | string | `InGame` or `Zoning` |
| `.Zone` | string | Zone short name; while zoning, the zone it's leaving |
| `.TargetID` | int | ID of that character's target, or 0 for none |
| `.Age` | int | Milliseconds since this client last published |
| `.Stale` | bool | True if `.Age` is over 3 seconds |

`.Spawn` is MacroQuest's real spawn if that character is in your zone, otherwise NULL.

A client that closes or crashes disappears from the posse straight away, without waiting for a
timeout. So does one that goes to character select, since it has no character then, and it joins again
on entering the game. A client whose game has stalled stays listed with `.Stale` true.

### Reading `Me` members

A member isn't MacroQuest's `character` type, which can only read the local character. You can read
values through it, but can't call methods such as `Me.Stand`. Use `/posse cmd Bob /stand` instead.

**These members are always ready**, and work on the first read:

- **Identity:** `Name`, `ID`, `Level`, `Class`, `Race`
- **Vitals:** `PctHPs`, `CurrentHPs`, `MaxHPs`, `PctMana`, `CurrentMana`, `MaxMana`, `PctEndurance`,
  `CurrentEndurance`, `MaxEndurance`
- **Condition:** `Combat`, `Casting`, `Sitting`, `Moving`, `Invis`, `Stunned`, `Feigning`, `Hovering`
- **Place:** `X`, `Y`, `Z`, `Heading`
- **Buffs:** `Buff[n or name]` and `Song[n or name]`, each with `ID` (the buff's slot, as in
  MacroQuest), `Spell.ID`, `Name` and `Duration`, plus `CountBuffs` and `CountSongs`
- **Pet:** `Pet.ID`, `Pet.Name`, `Pet.PctHPs`

`Class`, `Race`, `Casting` and `Heading` show the same text as `${Me}`'s. `Casting` is NULL when not
casting, and their own members, such as `Class.ShortName`, are read like any other member below.
`Duration` is a number of milliseconds left, not MacroQuest's `timestamp`, so it has no members such
as `TotalSeconds`, and it's NULL for a buff that doesn't wear off. `Spell` and `Casting` are
MacroQuest's real `spell` values. Your own entry reads what you last published, like anyone else's.

**Any other `Me` member works too, but not on the first read.** The first read returns NULL and asks
the owning client to start evaluating that path. Its value arrives shortly after and is then kept up
to date. Lua can read only the member names on a list built from MacroQuest's types, which covers
MacroQuest's own members but not ones another plugin adds. To have a value ready before you need it,
observe it first:

```
/posse observe Bob AltAbilityReady[Divine Arbitration]
/delay 5
/echo ${Posse[Bob].AltAbilityReady[Divine Arbitration]}
```

A path nothing has read for 30 seconds stops being evaluated, and observed paths stay active until
`/posse unobserve`. Paths are relative to `Me` and can't contain `${`.

### In a macro

```
| Heal any member of the posse below 50%
/declare name string local
/declare i int local
/for i 1 to ${Posse.Count}
    /varset name ${Posse.Members.Arg[${i}]}
    /if (!${Posse[${name}].Stale} && ${Posse[${name}].PctHPs} < 50) {
        /target ${name}
        /cast "Complete Heal"
    }
/next i

| Read the inbox
/while (${Posse.Inbox}) {
    /echo ${Posse.Inbox.From} says: ${Posse.Inbox.Text}
    /posse next
}
```

### In Lua

```lua
local mq = require('mq')

local bob = mq.TLO.Posse.Member('Bob')
if bob() and not bob.Stale() then
    print(bob.PctHPs(), bob.Buff('Clarity').Duration())
end
mq.cmd('/posse cmd Bob /sit')
```

Members outside the always-ready list return text until their first value arrives, then a number or
bool if that's what they are, so check for nil on those.

## Settings

`MQ2Posse.ini` in MacroQuest's `config` folder, under `[Server_Character]` or `[Default]`:

| Setting | Default | Meaning |
|---|---|---|
| `Posse` | `default` | Posse to join on entering the game |
| `AutoJoin` | `1` | `0` waits for `/posse join` |
| `AllowCommands` | `1` | `0` refuses `/posse cmd` from others, and senders see it time out |

## Limits

| Limit | Value |
|---|---|
| Clients in one posse | 64 |
| Posses per client | 1 |
| Message or command length | 512 bytes |
| Queued messages per client | 16; further sends fail with a chat message |
| Queued commands per client | 16; further sends fail with a chat message |
| Observed or read paths per client | 64 |
| Path length | 128 bytes |

## How it works

Clients share a block of named memory, one per server and posse name, with one slot per client. Each
client writes only to its own slot, and publishes its status once per game pulse. A reader gets the
latest published values straight from that slot whenever a macro asks, without any message passing.
A sequence counter makes sure a reader never sees a half-written update.

Messages and commands go into the target's slot and run on the target's next pulse, on the game
thread. So do `Me` paths that aren't in the always-ready list, because only the owning client can
evaluate them.

## How fast it is

How old a member's status was when read, measured in game with two clients over about 40 seconds each:

| | Client at its normal frame rate | Client limited to 10 fps |
|---|---|---|
| Average | 16 ms | 48 ms |
| Worst | 34 ms | 100 ms |

So status is never more than about one frame of the owning client behind, and the frame rate of the
client reading it doesn't matter. Publishing cost 27 to 30 µs per pulse on average, and 563 µs at worst.

These are still design estimates, not measurements: a command should start running within one frame
of the client receiving it, getting a value from shared memory should take well under a microsecond
on top of MacroQuest's usual cost of parsing a `${...}`, and each client's work per pulse should stay
about the same however many clients are in the posse.

Posse only removes delay between your clients. A client's own data is still only as current as what
the server has told it.

## Security

Anything running as your Windows user can read the shared memory, and could write commands into it.
Posse doesn't defend against programs on your own PC, so don't run clients alongside software you
don't trust. Set `AllowCommands=0` on any character that shouldn't accept commands.

## License

MIT. See [LICENSE](LICENSE).
