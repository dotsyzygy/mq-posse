# Working in this repo

- Never put functionality the API should provide into this plugin. If MQ2Posse needs a game value or
  action mq-emu doesn't have, add it to mq-emu (`c:\dev\repos\mq-rs`, `docs/adding-a-binding.md`),
  release it, and move this plugin to the new tag. Don't work around it with `${...}` paths here.
