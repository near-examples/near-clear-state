#! /usr/bin/env node
import { program } from "commander";
import { clearState } from "./commands/clearState.js";

program
  .command("clear-state")
  .description("deploy wasm file ")
  .option("-a,--account <account...>", "Add account name")
  .action(clearState);

program.parse();
let optionsOutput = program.opts();
export { optionsOutput };
