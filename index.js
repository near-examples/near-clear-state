#! /usr/bin/env node
import { program } from "commander";
import { clearState } from "./commands/clearState.js";

program
  .command("clear-state")
  .description("deploy wasm file and clear account state")
  .option("-a,--account <account.name>", "Set account name")
  .option("-n,--network <testnet/mainnet>", "Set the target network", 'testnet')
  .action(clearState);

program.parse();
let optionsOutput = program.opts();
export { optionsOutput };
