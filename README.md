# AgentMaster

Runs one process under a terminal of its own and hands control of that process
over a port: start it, read its window, type into it, resize the window, read what
it put on the clipboard, stop it, ask how it ended.

It is meant to run wherever the process itself runs — next to the supervisor on a
host, or inside a container with the port published — so that everything above it
drives a host process and a container process the same way.

## Parts

- `crates/daemon` — the program (`agent-master`), which owns the process.
- `crates/client` — the library everything else talks to it through.
- `crates/cli` — `amctl`, for driving one by hand.
- `crates/protocol` — the wire language, known only to the two sides above.

## By hand

```sh
agent-master --listen 127.0.0.1:7620    # or AGENT_MASTER_LISTEN in the environment

export AGENT_MASTER_ADDR=127.0.0.1:7620
amctl start --cwd /workdir --rows 80 --cols 100 --env TERM=xterm-256color -- claude
amctl window
amctl input 'hello'
printf '\r' | amctl input
amctl resize --rows 40 --cols 120
amctl state
amctl clipboard
amctl stop --grace-ms 5000
```

## Keys

Plain text goes as an argument; anything else travels as the bytes a terminal
sends for it.

```
up      $'\033[A'    Enter      $'\r'
down    $'\033[B'    Tab        $'\t'
right   $'\033[C'    Escape     $'\033'
left    $'\033[D'    Backspace  $'\177'
                     Ctrl-C     $'\003'
```

```sh
amctl input $'\033[B'          # bash and zsh
printf '\033[B' | amctl input  # anywhere
```

An application that has switched its cursor keys to the application mode wants
`$'\033OB'` for down, and so on for the other three.

`amctl window` writes the window as a terminal would paint it. On a terminal it
takes a screen of its own and follows the process until you press `q` — add
`--freeze` to show it as it is instead. Anywhere else it writes the bytes as
they are, once.
