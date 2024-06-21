# InuBot

A simple bot to produce organic load on EVM network.

Produce fixed TPS stress for a EVM RPC compatible chain.

- Can produce fixed TPS Load for given duration
- Use multiple accounts, fund them from master account & send funds back after duration is over.

## Install

```
cargo install --git https://github.com/AstarNetwork/inubot --branch main
```

or directly clone the repo and build it.

## Technical Overview

The core working of bot is pretty simple, if max tps to achieve is `X` then send `X` _valid_ `eth_sendRawTransaction` requests/s via RPC. The difficult part is making sure those RPC requests are valid, like nonce is correctly managed, gas price is sufficient, etc.

### Nonce Handling

As we know that for a any given EVM account the transactions are processed sequentially in respect to their nonces. The important thing to keep in mind is that nonces can't have gaps in them. If your current nonce is `N` but you send two new tx with nonces `N+2`and `N+3` then those txs will be stuck in mempool until tx with nonce `N+1` is available.

Usually when sending multiple tx we wait for previous tx to be included in a block before sending another, this ensures that onchain nonce is incremented successfully thus we can send next one without any issue. But this is different when we want to send multiple tx simultaneously without waiting for previous tx, in that case it require manual management of nonces.

To understand the problem better consider the scenarios

- Bot send 100 tx manually manually incrementing nonces, but 2nd tx rpc request got rejected or dropped (network issue) thus never making it to mempool, this will case all the next 97 txs (and the rest to follow) to be stuck in mempool until the tx with missing nonce is sent again.

### Components

- `GasPoller`:
  - Poll the RPC to fetch and update the latest gas price (`eth_gasPrice`)
  - Provides a watch channel to peek latest price
- `TransactionRandomizer`:
  - Build tx call data by randomly choosing from predefined types based on given probability
- `NonceManager`:
  - Manage nonces while keeping track of failed ones and always return lowest available, based on ideas from - https://ethereum.stackexchange.com/a/40286
- `Actor`
  - Actor represent an EVM account with it's own nonce management using (`NonceManager`)
  - Fetch next tx from randomizer (`TransactionRandomizer`), apply nonce to it, send it to rate controller
- `ActorManager`
  - Orchestrate the actors lifecycle,
    - Fund the actors account from master
    - Spawn the actors tasks to send tx
    - On shutdown, teardown actors and wait for pending tx to clear
    - Attempt to return the funds back to master
- `RateController`: Send tx via rpc on a fixed rate

## Configuration

### CLI Interface

```
A simple stress bot for producing organic load on EVM network.

Usage: inu [OPTIONS] <COMMAND>

Commands:
  run       Start sending the transactions to network
  withdraw  Withdraw the funds back from actors account to master
  metrics   Only run the chain metrics
  mnemonic  Generate a random account
  deploy    Deploy the helper contract
  help      Print this message or the help of the given subcommand(s)

Options:
  -c, --config <CONFIG>  Path to the inu configuration file
  -h, --help             Print help
  -V, --version          Print version

Network Options:
  -r, --rpc-url <RPC_URL>        RPC url of the network
  -n, --network <NETWORK>        Name of the network(from config file)
  -b, --block-time <BLOCK_TIME>  Override block time of the network (default: 2s)

Global Options:
      --tx-timeout <TX_TIMEOUT>          Timeout for each transaction (default: 5min)
      --tps-per-actor <TPS_PER_ACTOR>    Per actor TPS (default: 50)
      --gas-multiplier <GAS_MULTIPLIER>  Gas multiplier (default: 1.5)
```

### Config File

Inubot also supports an **optional** config file, and by default looks for it in same dir with name `inu_config.toml` or you can provide it via cli.

```toml
# Maximum time to wait for a tx to get confirmed
# For a very high tps value, there might be congestion in mempool (more tx incoming than block rate)
# then timeout should be increased accordingly
tx_timeout = "5min"
# Max TPS for a single actor
# For example, if TPS is 100 then 2 actor will be spawned if value if 50
tps_per_actor = 50
# multiplier to apply on gas price received from RPC
# this to account for fluctuation of gas price
gas_multiplier = 1.5

# Mapping of type of transactions to enable with their probability
# - To disable certain type remove them from mapping
# - The probability values can be anything greater than 0
# - By default all of the types are enabled with probability given below
# NOTE: The deploy and mints needs significantly more gas, thus max tps per block
# will be considerably low if they are enabled with high probability. This will cases
# tx to take MUCH MUCH longer to confirm, so increase `tx_timeout` accordingly, otherwise
# there would a false tx timeouts
[transactions]
Transfer = 0.95
ERC20Mint = 0.02
ERC721Mint = 0.015
ERC1155Mint = 0.015
ERC20Deploy = 0.01
ERC721Deploy = 0.005
ERC1155Deploy = 0.005

# Networks configuration
[networks.my-network]
# avg block time, only used to calculate tps of the first block
block_time = "1s"
rpc_url = "ws://localhost:12345"
# address of helper contract
organic_address = "0x9f7ddec24dd21b41d3cc87330f860e4d8dad10ed"
default = true

[networks.local]
rpc_url = "ws://localhost:8545"

```

### `INU_` environment variable

It also supports providing parameters from `INU_` prefixed envs, for example you can set `tx_timeout` via `INU_TX_TIMEOUT=20s` as well.

**NOTE**: The if a value if provided via all three ways then precedence is CLI > ENV > Config

## User Guide

### 0. Setup

- https://www.rust-lang.org/tools/install
- [Install bot](#Install)

### 1. Prepare Mnemonic

The bot requires an mnemonic from which it derives **master** (index 0) and subsequent actor accounts.

- The master account is used to fund the actor accounts equally on every run

Generate the accounts and fund them

- Run `inu mnemonic` subcommand to generate random mnemonic
- Save the mnemonic in `.env` file with key `INU_MNEMONIC`
- Fund the master account with appropriate ETH

### 2. Run the bot

- Start bot to produce 100 TPS load for 20s, and enable to print chain metrics

  ```bash
  inu run --max-tps 100 --duration 20s --metrics --rpc-url http://localhost:8545
  ```

- Start bot to produce 100 TPS load until stopped, and without printing chain metrics

  ```bash
  inu run --max-tps 100 --rpc-url http://localhost:8545
  ```

- Start bot to produce 100 TPS load until stopped, and pick network from config file (`inu_config.toml`)

  ```bash
  inu run --max-tps 100 --network local
  ```

- Start bot to produce 100 TPS load until stopped, and pick the default network from config file specified via cli

  ```bash
  inu run --max-tps 100 --config-file ../my_inu_config.toml
  ```

- Print metrics only, and pick network from config file (`inu_config.toml`)

  ```bash
  inu metrics --network local
  ```

- Only attempt to withdraw fund from actors, `max_tps=100` with `tps_per_actor=50` means 2 actors

  ```bash
  inu withdraw --max-tps 100 --network local
  ```

- Deploy helper contract (you can copy and save it to config file to prevent deploying on every run)
  ```bash
  inu deploy --rpc-url http://localhost:8545
  ```

## Block Gas Limit and `tx_timeout`

The `tx_timeout` is very important parameter. It specifies how long to wait for a tx to get included in a block before assuming it's stuck in mempool. There could 2 ways timeout can be triggered,

- The tx is genuinely stucked in mempool due to low gas price, this is highly unlikely in L2s
- There is a lot of congestion in mempool thus tx is taking longer to get confirmed, this one is very likely if block gas limit are the bottleneck

Consider the scenarios,

- With the default transaction type probabilities (mostly transfer and very low mint and deploy txs) and assuming block gas limit is 30M, the max transaction per block is around 800 due to block gas limit.
- Considering the block time is 4s, then that means the max tps can be only **200 tps**
- So if you start the bot with TPS > 200 then the network can't keep up and thus there would be congestion in mempool and ultimately `tx_timeout` will hit on some tx leading to false errors

To conclude, test the network's max tps by slowly increasing the tps, or make `tx_timeout` very high so it's essentially disabled.
