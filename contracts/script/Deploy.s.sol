// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.13;

import {Script, console} from "forge-std/Script.sol";
import {Organic} from "../src/Organic.sol";

contract CounterScript is Script {
    function run() public {
        vm.startBroadcast(vm.envUint("PRIVATE_KEY"));

        // Deploy the Organic contract
        Organic organic = new Organic();

        // Broadcast the transaction
        vm.stopBroadcast();
    }
}
