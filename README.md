# near-clear-state

## How to use

### Things you'll need

First you'll need to get `near-cli` you can install by running

```bash
npm i -g near-cli
```

You can view the all the state keys (for testnet for example) in your account with

```bash
near view-state <account-name.testnet>
```

## Steps to Clear Your Account State

### Step 1 Login With NEAR CLI

This will store a full access key locally on your machine. Select the account you wish to clear the state of

```bash
near login
```

### Step 2 Clone this Repo!

`git clone https://github.com/doriancrutcher/near-clear-state.git`

### Step 3 Install Dependencies

```bash
cd near-clear-state && npm i
```

### Step 4 Clear Your State

```bash
near-clear-state clear-state --account <account-name.testnet>
```
