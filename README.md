# MQ2Posse

A MacroQuest plugin that lets EverQuest clients on the same PC see each other's status and send
each other commands. It adds a `Posse` TLO to macros and Lua.

> **Status: early.** Joining a posse, status, the `Posse` TLO and `/posse list`, `join`, `leave` and
> `stats` are built and have run on two clients. The commands that run on your clients have run in game
> on three clients, including `/pc` to a name, `${Posse.LastCommand}` and channels. Reading `Me` members
> outside the always-ready list isn't built, and this README describes it as designed.
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
- **Sends commands:** to one character, your group, a named channel or the whole posse.
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

| Command                        | What it does                                                                                                    |
| ------------------------------ | --------------------------------------------------------------------------------------------------------------- |
| `/posse help` | Lists the help topics |
| `/posse help all` | Lists every command |
| `/posse help <topic>` | Lists the commands for that topic: posse, channel, settings or command |
| `/posse list`                  | Lists members with their state and data age, as does /posse alone                                               |
| `/posse join <name>`           | Leaves the current posse and joins that one, until you next go to character select                              |
| `/posse leave`                 | Leaves the posse until the next /posse join                                                                     |
| `/posse channel list`          | Lists every channel and who's in it, as does /posse channel alone                                               |
| `/posse channel create <name>` | Creates a channel, which stays until it's deleted                                                               |
| `/posse channel delete <name>` | Deletes a channel once nobody online is in it                                                                   |
| `/posse channel join <name>`   | Puts this character in a channel, and keeps it there                                                            |
| `/posse channel leave <name>`  | Takes this character out of a channel, and keeps it out                                                         |
| `/posse debug on`              | Logs debugging information to the MQ console for this character                                                 |
| `/posse debug off`             | Turns off debugging                                                                                             |
| `/posse allowcommands true` | Runs commands other clients send, and keeps doing so for this character |
| `/posse allowcommands false` | Refuses commands other clients send, and keeps doing so for this character |
| `/posse stats`                 | Shows publishing costs for client per pulse, member status age, and how long commands took to reach this client |
| `/posse stats reset`           | Reset the stats                                                                                                 |

`/posse help` lists the commands that work so far, including the ones below.

### Running a command on your other clients

In these names, `p` means posse, `c` command, `g` group, `r` raid, `z` zone, `n` a named channel, and a
final `a` all including you. `/pc` alone is to one character, so `/pca` is to all of them except you, and
`/pcaa` to all of them including you.

| Command                     | What it does                                                                  |
| --------------------------- | ----------------------------------------------------------------------------- |
| `/pc <name> <command>`      | Runs a command on that character, which can be you                            |
| `/pca <command>`            | Runs a command on every connected client except you                           |
| `/pcaa <command>`           | Runs a command on every connected client, including you                       |
| `/pgc <command>`            | Runs a command on every client in your group except you                       |
| `/pgca <command>`           | Runs a command on every client in your group, including you                   |
| `/prc <command>`            | Runs a command on every client in your raid except you, your group included   |
| `/prca <command>`           | Runs a command on every client in your raid, including you                    |
| `/pzc <command>`            | Runs a command on every client in your zone except you                        |
| `/pzca <command>`           | Runs a command on every client in your zone, including you                    |
| `/pnc <channel> <command>`  | Runs a command on every client in that channel except you                     |
| `/pnca <channel> <command>` | Runs a command on every client in that channel, including you if you're in it |

For example:

```
/pca /stand
/pgca /sit
/pzc /target id ${Me.ID}
/pc Bob /sit
/pnca healers /stand
```

As usual in MacroQuest, `${...}` in a command is expanded on the client that sends it. Here,
`${Me.ID}` is the sender's ID.

- The command must start with `/`.
- Who's in your group or raid is decided by your client when you send. Outside a group or raid,
  `/pgc`, `/pgca`, `/prc` and `/prca` say so and run nothing, on you included.
- `/prc` and `/prca` don't fall back to your group if you're grouped but not in a raid.
- Your zone means the same instance of it, and leaves out clients on a load screen.
- A client on a load screen runs what it was sent when it arrives. If that's over 5 seconds after
  the command was sent, it doesn't run it, and says so in its chat.
- A client with `AllowCommands=false` refuses the command and says so in its own chat. The sender sees it in
  `${Posse.LastCommand.Refused}`.
- `/pc` can name your own character. Naming a character not in the posse says so and sends
  nothing, as does sending to a channel that doesn't exist.

### Finding out what happened to a command

`${Posse.LastCommand}` follows the last command you sent with any of these, and each target of it is in
one of its counts:

```
/pc Bob /sit
/delay 6s ${Posse.LastCommand.Pending}==0
/if (${Posse.LastCommand.Done}!=${Posse.LastCommand.Targets}) /echo Bob didn't run it
```

A client that drops a command you sent, because it reached it over 5 seconds after you sent it, tells you
in your chat, for any command and not only the last. So does one that lost commands you sent, by not
reading them before you'd sent 16 more. It can't tell whether those were addressed to it, so they may not
all have been meant for it.

### Channels

A channel is a name for some of your clients, shared by the whole posse: any client can create one, join
one, and send to one. Create it once, then have each client join:

```
/posse channel create healers
/pc Bob /posse channel join healers
/pnc healers /sit
```

- A channel and each client's place in it are saved in `MQ2Posse.ini`, so they come back when the clients
  do. A channel stays with nobody in it, until it's deleted.
- Channels belong to one posse on one server. Names are 1 to 31 letters, digits, `-` or `_`, in any case.
- Joining a channel that doesn't exist says so instead of creating it, so that a typo can't make a second
  channel. For the same reason, `/pnc` to a channel that doesn't exist doesn't send anything.
- To take another character out of a channel, run the command on it:
  `/pc Bob /posse channel leave healers`.
- A channel with members online can't be deleted. A member who's offline when it's deleted keeps it in its
  `Channels` setting, and is back in it if a channel of that name is created again.

## The Posse TLO

The main members are here. [docs/tlo.md](docs/tlo.md) has every member that exists today.

| Member                                    | Type    | Meaning                                                                                    |
| ----------------------------------------- | ------- | ------------------------------------------------------------------------------------------ |
| `${Posse}`                                | string  | The posse's name, or NULL if not in one                                                    |
| `${Posse.Count}`                          | int     | Members currently online, including you                                                    |
| `${Posse.Members}`                        | string  | Their names, separated by spaces                                                           |
| `${Posse.Member[Bob]}` or `${Posse[Bob]}` | member  | That member, or NULL if not online                                                         |
| `${Posse.Channels}`                       | string  | The names of the posse's channels, separated by spaces                                     |
| `${Posse.Channel[healers]}`               | channel | That channel's name, or NULL if there's no such channel                                    |
| `${Posse.Channel[healers].Count}`         | int     | Its members currently online, including you if you're in it                                |
| `${Posse.Channel[healers].Members}`       | string  | Their names, separated by spaces                                                           |
| `${Posse.LastCommand}`                    | string  | The last command you sent, or empty if you haven't sent one since joining                  |
| `${Posse.LastCommand.Targets}`            | int     | How many it was sent to, including you if it ran on you too                                |
| `${Posse.LastCommand.Pending}`            | int     | Targets that haven't run it yet, in the 5 seconds it has to arrive                         |
| `${Posse.LastCommand.Done}`               | int     | Targets that have run it                                                                   |
| `${Posse.LastCommand.Refused}`            | int     | Targets that refused it because of `AllowCommands=false`, or where MacroQuest wouldn't take it |
| `${Posse.LastCommand.Dropped}`            | int     | Targets that got it too late to run it                                                     |
| `${Posse.LastCommand.TimedOut}`           | int     | Targets not heard from after 5 seconds: stalled, on a long load screen, or gone            |
| `${Posse.LastCommand.Lost}`               | int     | Commands targets have reported losing since you sent this one                              |

`Done` means the target's client ran the command, not that it worked in game: `/cast` can still
fizzle. A target on a long load screen moves from `TimedOut` to `Dropped` when it arrives. `Lost` counts
earlier commands, as the last one is never overwritten, and they may have been addressed to other clients.

A member has `${Me}`'s members, under the same names, so `${Posse[Bob].PctHPs}` is Bob's
`${Me.PctHPs}`. It also has these, which `${Me}` doesn't:

| Member       | Type   | Meaning                                              |
| ------------ | ------ | ---------------------------------------------------- |
| `.GameState` | string | `InGame` or `Zoning`                                 |
| `.Zone`      | string | Zone short name; while zoning, the zone it's leaving |
| `.TargetID`  | int    | ID of that character's target, or 0 for none         |
| `.Age`       | int    | Milliseconds since this client last published        |
| `.Stale`     | bool   | True if `.Age` is over 3 seconds                     |

A member's `.Channels` is the channels it's in, separated by spaces.

`.Spawn` is MacroQuest's real spawn if that character is in your zone, otherwise NULL.

A client that closes or crashes disappears from the posse straight away, without waiting for a
timeout. So does one that goes to character select, since it has no character then, and it joins again
on entering the game. A client whose game has stalled stays listed with `.Stale` true.

### Reading `Me` members

A member isn't MacroQuest's `character` type, which can only read the local character. You can read
values through it, but can't call methods such as `Me.Stand`. Use `/pc Bob /stand` instead.

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
read it once first and ignore the result:

```
/varset ignored ${Posse[Bob].AltAbilityReady[Divine Arbitration]}
/delay 5
/echo ${Posse[Bob].AltAbilityReady[Divine Arbitration]}
```

A path nothing has read for 30 seconds stops being evaluated, but its last value is kept. The next
read returns that value, which may be out of date, and starts evaluating the path again. Paths are
relative to `Me` and can't contain `${`.

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
```

### In Lua

```lua
local mq = require('mq')

local bob = mq.TLO.Posse.Member('Bob')
if bob() and not bob.Stale() then
    print(bob.PctHPs(), bob.Buff('Clarity').Duration())
end
mq.cmd('/pc Bob /sit')
```

Members outside the always-ready list return text until their first value arrives, then a number or
bool if that's what they are, so check for nil on those.

## Settings

`MQ2Posse.ini` in MacroQuest's `config` folder, under `[Server_Character]` or `[Default]`:

| Setting            | Default   | Meaning                                                                                                                                            |
| ------------------ | --------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Posse`            | `default` | Posse to join on entering the game                                                                                                                 |
| `AutoJoin`         | `1`       | `0` waits for `/posse join`                                                                                                                        |
| `AllowCommands`    | `true`    | `false` refuses commands from others, saying so in this client's chat. `/posse allowcommands` sets this under `[Server_Character]` |
| `Debug`            | `off`     | `on` says in chat what this client sends and receives. `/posse debug` sets this under `[Server_Character]`, creating the file if needed            |
| `Channels`         | none      | The channels this character is in, separated by commas. `/posse channel join` and `leave` set this, only under `[Server_Character]`. In a posse without one of them, that one is ignored and stays saved |

The channels themselves are the keys of a section of their own, `[Channels_<server>_<posse>]`. All clients
need the same `config` folder to share what's saved there; running one MacroQuest for all of them does that.

Errors always show in chat, whether or not `Debug` is on.

## Limits

| Limit                                              | Value                                                                             |
| -------------------------------------------------- | --------------------------------------------------------------------------------- |
| Clients in one posse                               | 64                                                                                |
| Posses per client                                  | 1                                                                                 |
| Command length                                     | 2047 bytes, the most MacroQuest takes in a line                                   |
| Commands a client can send before others read them | 16; past that the oldest are lost, and a client that lost any says so in its chat |
| Age of a command that will still run               | 5 seconds                                                                         |
| Channels in one posse                              | 64                                                                                |
| Channels per client                                | 16                                                                                |
| Channel name length                                | 31 bytes                                                                          |
| Read paths per client                              | 64                                                                                |
| Path length                                        | 128 bytes                                                                         |

## How it works

Clients share a block of named memory, one per server and posse name, with one slot per client. Each
client writes only to its own slot, and publishes its status once per game pulse. A reader gets the
latest published values straight from that slot whenever a macro asks, without any message passing.
A sequence counter makes sure a reader never sees a half-written update.

A command goes into a log in the sender's own slot, addressed to the slots it's for, and each target
picks it up on its next pulse and runs it on the game thread, then answers in its own slot, where the
sender reads it. Channels are in one table that all clients share, and each client lists the ones it's in
as part of its status, so a client that goes away is out of them at once. `Me` paths that aren't in the
always-ready list will go into the owning client's slot and be answered the same way, because only that
client can evaluate them.

## How fast it is

How old a member's status was when read, measured in game with two clients over about 40 seconds each:

|         | Client at its normal frame rate | Client limited to 10 fps |
| ------- | ------------------------------- | ------------------------ |
| Average | 16 ms                           | 48 ms                    |
| Worst   | 34 ms                           | 100 ms                   |

So status is never more than about one frame of the owning client behind, and the frame rate of the
client reading it doesn't matter. Publishing cost 27 to 30 µs per pulse on average, and 563 µs at worst.

Commands took 2 to 103 ms to reach another client, over 7 samples in game with three clients. A command
that goes to the server, such as `/say`, then takes as long as the server does.

These are still design estimates, not measurements: getting a value from shared memory should take well under a microsecond
on top of MacroQuest's usual cost of parsing a `${...}`, and each client's work per pulse should stay
about the same however many clients are in the posse.

Posse only removes delay between your clients. A client's own data is still only as current as what
the server has told it.

## Security

Anything running as your Windows user can read the shared memory, and could write commands into it.
Posse doesn't defend against programs on your own PC, so don't run clients alongside software you
don't trust. Set `AllowCommands=false`, or use `/posse allowcommands false`, on any character that shouldn't accept commands.

## License

MIT. See [LICENSE](LICENSE).
