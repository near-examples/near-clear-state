import * as nearAPI from "near-api-js";

const os = import("os");
const path = import("path");
const fs = import("fs");
const { connect } = nearAPI;

let near;
let config;
let account;

// set up near
const initiateNear = async () => {
  const { keyStores } = nearAPI;
  const homedir = (await os).homedir();
  const CREDENTIALS_DIR = ".near-credentials";

  const credentialsPath = (await path).join(homedir, CREDENTIALS_DIR);
  (await path).join;
  const keyStore = new keyStores.UnencryptedFileSystemKeyStore(credentialsPath);

  config = {
    networkId: "testnet",
    keyStore,
    nodeUrl: "https://rpc.testnet.near.org",
    walletUrl: "https://wallet.testnet.near.org",
    helperUrl: "https://helper.testnet.near.org",
    explorerUrl: "https://explorer.testnet.near.org",
  };

  near = await connect(config);
};
export async function clearState(optionsObject) {
  // console.log(optionsOutput);
  await initiateNear();
  console.log("account name is ", optionsObject.account[0]);
  account = await near.account(optionsObject.account[0]);

  let state = await account.viewState("", { finality: "final" });

  state = state.map(({ key, value }) => ({
    key: key.toString("base64"),
    value: value.toString("base64"),
  }));

  let keys = state.map((el) => {
    return el.key;
  });

  console.log(keys);

  // Deploy contract onto account to clear state
  if (account) {
    // deploys contract
    const response = await account.deployContract(
      (await fs).readFileSync("../contractWasm/state_cleanup.wasm")
    );

    console.log("deploying contract. Response:", response);

    // Contract object to retreive methods
    const contract = new nearAPI.Contract(
      account, // the account object that is connecting
      "example-contract.testnet",
      {
        // name of contract you're connecting to
        viewMethods: [], // view methods do not change state but usually return a value
        changeMethods: ["clean"], // change methods modify state
        sender: account, // account object to initialize and sign transactions.
      }
    );

    return await contract.account.functionCall({
      contractId: optionsObject.account[0],
      methodName: "clean",
      args: {
        keys: keys,
      },
      gas: "300000000000000",
    });
  }
}

// module.exports = deployCode;
