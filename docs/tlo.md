# The Posse TLO

Every member the `Posse` TLO has today, for macros and Lua. The [README](../README.md) has the commands
these go with, and [status.md](status.md) has what's still being built.

## Reading these tables

- **Types** are what MacroQuest sees. `int` is `int64` for a number too big for it. `spell` and `spawn` are
  MacroQuest's own types, with all their usual members. The `posse_*` types are described below.
- **NULL** is what a read gives when the thing it asks for isn't there, and `nil` in Lua.
- **Character and channel names** match in any case, so `${Posse[bob]}` is `${Posse[Bob]}`.
- **How fresh:** a client publishes its status each pulse, so values are as old as `.Age` says. The list of
  who's online is rebuilt every 100 ms, so a member who has just joined can take that long to appear.

## `Posse`

| Form | Type | Meaning |
|---|---|---|
| `${Posse}` | `posse` | The posse. NULL if this client isn't in one, which makes every read below NULL too |
| `${Posse[Bob]}` | `posse_member` | The same as `${Posse.Member[Bob]}` |

## `posse`

Shows as the posse's name.

| Member | Type | Meaning |
|---|---|---|
| `.Count` | `int` | Members online, including you |
| `.Members` | `string` | Their names, separated by spaces, sorted by name |
| `.Member[Bob]` | `posse_member` | That member, or NULL if not online |
| `.Channels` | `string` | The names of the posse's channels, separated by spaces, sorted by name. Empty if there are none |
| `.Channel[healers]` | `posse_channel` | That channel, or NULL if there's no such channel |
| `.LastCommand` | `posse_last_command` | The last command you sent. Not NULL when you haven't sent one: it's empty and all its counts are 0 |

## `posse_member`

Shows as the member's name. Members named after one of `${Me}`'s read the same as that character's
`${Me}`, except as noted. Your own entry reads what you last published, like anyone else's.

Every member here is NULL once that character is no longer online. While it's on a load screen, these keep
their last values, and `.GameState` says so.

Who and where:

| Member | Type | Meaning |
|---|---|---|
| `.Name` | `string` | The character's name |
| `.ID` | `int` | Its spawn ID |
| `.Level` | `int` | Its level |
| `.Class` | `string` | Its class's name. Only text for now, so `.Class.ShortName` doesn't work |
| `.Race` | `string` | Its race's name. Only text for now, like `.Class` |
| `.Zone` | `string` | Its zone's short name. While zoning, the zone it's leaving |
| `.X` | `float` | Its X position |
| `.Y` | `float` | Its Y position |
| `.Z` | `float` | Its Z position |
| `.Heading` | `string` | The compass point it's facing, such as `NNE`. Only text for now, so `.Heading.Degrees` doesn't work |
| `.TargetID` | `int` | The spawn ID of its target, or 0 for none. `${Me}` doesn't have this |
| `.Spawn` | `spawn` | MacroQuest's spawn for it, or NULL unless it's in game in your zone and the same instance of it |

Vitals:

| Member | Type | Meaning |
|---|---|---|
| `.PctHPs` | `int` | Hit points, as a percentage |
| `.CurrentHPs` | `int` | Hit points |
| `.MaxHPs` | `int` | Maximum hit points |
| `.PctMana` | `int` | Mana, as a percentage, or 0 for a class with no mana |
| `.CurrentMana` | `int` | Mana |
| `.MaxMana` | `int` | Maximum mana |
| `.PctEndurance` | `int` | Endurance, as a percentage, or 0 with no endurance |
| `.CurrentEndurance` | `int` | Endurance |
| `.MaxEndurance` | `int` | Maximum endurance |

Condition:

| Member | Type | Meaning |
|---|---|---|
| `.Combat` | `bool` | In combat |
| `.Sitting` | `bool` | Sitting |
| `.Moving` | `bool` | Moving |
| `.Invis` | `bool` | Invisible |
| `.Stunned` | `bool` | Stunned |
| `.Feigning` | `bool` | Feigning death |
| `.Hovering` | `bool` | Hovering, after dying |
| `.Casting` | `spell` | The spell being cast, or NULL if not casting |

Buffs, pet and channels:

| Member | Type | Meaning |
|---|---|---|
| `.Buff[n or name]` | `posse_buff` | The buff in that slot, counted from 1, or the first with that spell's exact name. NULL if there's none |
| `.Song[n or name]` | `posse_buff` | The same for songs |
| `.CountBuffs` | `int` | How many buffs it has |
| `.CountSongs` | `int` | How many songs it has |
| `.Pet` | `posse_pet` | Its pet. Not NULL when it has none: it shows `NO PET` |
| `.Channels` | `string` | The channels it's in, separated by spaces. Empty if none. `${Me}` doesn't have this |

The state of its data, which `${Me}` doesn't have:

| Member | Type | Meaning |
|---|---|---|
| `.GameState` | `string` | `InGame` or `Zoning`. Not `.State`, which on `${Me}` means `STAND`, `SIT` and so on |
| `.Age` | `int` | Milliseconds since this client last published |
| `.Stale` | `bool` | True if `.Age` is over 3 seconds |

## `posse_pet`

Shows as the pet's name, or `NO PET`.

| Member | Type | Meaning |
|---|---|---|
| `.ID` | `int` | Its spawn ID, or 0 for no pet |
| `.Name` | `string` | Its internal name, as `${Me.Pet.Name}` shows it, or `NO PET` |
| `.PctHPs` | `int` | Its hit points, as a percentage, or 0 for no pet |

## `posse_buff`

A buff or song. Shows as its spell's name.

| Member | Type | Meaning |
|---|---|---|
| `.ID` | `int` | Its slot, counted from 1, as in MacroQuest |
| `.Name` | `string` | Its spell's name |
| `.Spell` | `spell` | Its spell |
| `.Duration` | `int` | Milliseconds left, or NULL if it doesn't wear off. Unlike `${Me.Buff[x].Duration}` this is a plain number, so it has no members such as `.TotalSeconds` |

## `posse_channel`

Shows as the channel's name, as it was created.

| Member | Type | Meaning |
|---|---|---|
| `.Count` | `int` | Its members online, including you if you're in it |
| `.Members` | `string` | Their names, separated by spaces, sorted by name. Empty if nobody online is in it |

## `posse_last_command`

The last command you sent with `/pc` or any of the others. Shows as that command. It starts
empty each time this client joins a posse.

Each target is counted in exactly one of `.Pending`, `.Done`, `.Refused`, `.Dropped` and `.TimedOut`, so they
add up to `.Targets`. A macro waits for an answer like this:

```
/pc Bob /sit
/delay 6s ${Posse.LastCommand.Pending}==0
/if (${Posse.LastCommand.Done}!=${Posse.LastCommand.Targets}) /echo Bob didn't run it
```

| Member | Type | Meaning |
|---|---|---|
| `.Targets` | `int` | How many it was sent to, including you if it ran on you too |
| `.Pending` | `int` | Targets that haven't answered, in the 5 seconds it has to arrive |
| `.Done` | `int` | Targets that have run it. That's not to say it worked in game: `/cast` can still fizzle |
| `.Refused` | `int` | Targets that refused it because of `AllowCommands=false`, or where MacroQuest wouldn't take it |
| `.Dropped` | `int` | Targets that got it too late to run it |
| `.TimedOut` | `int` | Targets not heard from after 5 seconds: stalled, on a long load screen, or gone. One that gets to it later moves to `.Dropped` |
| `.Lost` | `int` | A count of commands and not targets: earlier commands that targets have reported losing since this one was sent. They may have been addressed to other clients |

## Not built yet

These are in the [README](../README.md) as the plan, but read as NULL today. Their state is in
[status.md](status.md).

- Any other `${Me}` member on a member, such as `${Posse[Bob].AltAbilityReady[...]}`, from phase 3.
