# InuBot - WIP

Produce fixed TPS stress for a EVM RPC compatible chain.

- Can produce fixed TPS Load for given duration
- Use multiple accounts, fund them from master account & send funds back after duration is over.

## TODO

- [x] Reduce nonce retry overhead
- [ ] Add complex evm tx (erc20, dex, etc) in builder
- [x] Add better error handling and logs
- [x] Add support for dynamic gas price
- [x] Add support for metrics

## Binaries

### `bip39`

Generate random nnemonic and print first 5 address. This will be helpful in setting the inu bot later.

```
./target/release/bip39
```

Just run it and it'll print the info.

### `tps`

Monitor the TPS and block utilization using JSON RPC (both http & ws supported)

```
Usage: tps [OPTIONS] --rpc-url <RPC_URL>

Options:
  -r, --rpc-url <RPC_URL>        Sets the RPC URL
  -b, --block-time <BLOCK_TIME>  Sets the block time in seconds [default: 2]
  -h, --help                     Print help
  -V, --version                  Print version
```

### `inu`

The stress bot

```
Usage: inu [OPTIONS] --rpc-url <RPC_URL> --max-tps <MAX_TPS>

Options:
  -r, --rpc-url <RPC_URL>    Sets the RPC URL
  -m, --max-tps <MAX_TPS>    Sets the maximum TPS
  -d, --duration <DURATION>  Sets the duration in seconds
  -h, --help                 Print help
  -V, --version              Print version
```

## How to run?

### Prepare

1. Install Rust & build project

   ```bash
   cargo build --release
   ```

   This will produce 3 binaries in `target/release` folder

2. Generate new accounts and fund them

   - Run `bip39` to print accounts data
   - Save the nnemonic in `.env` file
   - Fund the master account with appropriate ETH

3. Spawn `tps` executable in terminal to monitor chain stats
4. Spawn `inu` executable in separate terminal to produce load
