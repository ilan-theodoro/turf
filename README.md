# turf

**turf** - *turm but friendlier*.

A text-based user interface (TUI) for the [Slurm Workload Manager](https://slurm.schedmd.com/), which provides a convenient way to manage your cluster jobs.

Maintained by **Ilan Theodoro**.

This is a fork of [turm](https://github.com/kabouzeid/turm) by Karim Abou Zeid.

<img alt="turf demo" src="https://user-images.githubusercontent.com/7303830/228503846-3e5abc04-2c1e-422e-844b-d12ca097403a.gif" width="100%" />

`turf` accepts the same options as `squeue` (see [man squeue](https://slurm.schedmd.com/squeue.html#SECTION_OPTIONS)). Use `turf --help` to get a list of all available options.

## Installation

Build `turf` from source:

```shell
# With cargo.
cargo install --git https://github.com/ilan-theodoro/turf

# Or clone and build locally.
git clone https://github.com/ilan-theodoro/turf
cd turf
cargo build --release
```

### Shell Completion (optional)

#### Bash

In your `.bashrc`, add the following line:
```bash
eval "$(turf completion bash)"
```

#### Zsh

In your `.zshrc`, add the following line:
```zsh
eval "$(turf completion zsh)"
```

#### Fish

In your `config.fish` or in a separate `completions/turf.fish` file, add the following line:
```fish
turf completion fish | source
```

## How it works

`turf` obtains information about jobs by parsing the output of `squeue`.
The reason for this is that `squeue` is available on all Slurm clusters, and running it periodically is not too expensive for the Slurm controller ( particularly when [filtering by user](https://slurm.schedmd.com/squeue.html#OPT_user)).
In contrast, Slurm's C API is unstable, and Slurm's REST API is not always available and can be costly for the Slurm controller.
Another advantage is that we get free support for the exact same CLI flags as `squeue`, which users are already familiar with, for filtering and sorting the jobs.

### Resource usage

TL;DR: `turf` ≈ `watch -n2 squeue` + `tail -f slurm-log.out`

Special care has been taken to ensure that `turf` is as lightweight as possible in terms of its impact on the Slurm controller and its file I/O operations.
The job queue is updated every two seconds by running `squeue`.
When there are many jobs in the queue, it is advisable to specify a single user to reduce the load on the Slurm controller (see [squeue --user](https://slurm.schedmd.com/squeue.html#OPT_user)).
`turf` updates the currently displayed log file on every inotify modify notification, and it only reads the newly appended lines after the initial read.
However, since inotify notifications are not supported for remote file systems, such as NFS, `turf` also polls the file for newly appended bytes every two seconds.
