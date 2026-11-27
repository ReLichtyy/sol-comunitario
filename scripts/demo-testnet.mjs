// Carga una demo real en testnet: comité 2 de 3, planes, familias que firman,
// instalación y la primera lectura del medidor.
//
// Uso:  node scripts/demo-testnet.mjs <CONTRACT_ID> [paso inicial, 1 por defecto]
// Requiere las identidades sc-* creadas con `stellar keys generate --fund`.
// Las llaves secretas se leen del Stellar CLI local y nunca se guardan.
import { execSync } from "node:child_process";
import {
  Account, Address, Contract, Keypair, Networks, Operation, TransactionBuilder,
  authorizeEntry, nativeToScVal, rpc, xdr,
} from "@stellar/stellar-sdk";

const CONTRACT = process.argv[2];
const FROM = Number(process.argv[3] ?? 1);
if (!CONTRACT) throw new Error("Uso: node scripts/demo-testnet.mjs <CONTRACT_ID>");
const XLM_SAC = "CDLZFC3SYJYDZT7K67VZ75HPJVIEUVNIXF47ZG2FB2RMQQVU2HHGCYSC";
const XLM = 10_000_000n; // stroops por XLM
const server = new rpc.Server("https://soroban-testnet.stellar.org");
const passphrase = Networks.TESTNET;

const kp = name => Keypair.fromSecret(execSync(`stellar keys show ${name}`).toString().trim());
const K = Object.fromEntries(
  ["marta", "luis", "sofia", "ana", "beto", "solartico"].map(n => [n, kp(`sc-${n}`)]),
);
const byAddress = Object.fromEntries(Object.values(K).map(k => [k.publicKey(), k]));

const sym = s => xdr.ScVal.scvSymbol(s);
// Los structs de Soroban son mapas con llaves ordenadas alfabéticamente.
const struct = fields => xdr.ScVal.scvMap(
  Object.keys(fields).sort().map(k => new xdr.ScMapEntry({ key: sym(k), val: fields[k] })),
);
const addr = k => new Address(k.publicKey()).toScVal();
const str = s => nativeToScVal(s, { type: "string" });
const u32 = n => nativeToScVal(n, { type: "u32" });
const u64 = n => nativeToScVal(n, { type: "u64" });
const i128 = n => nativeToScVal(n, { type: "i128" });

async function invoke(source, method, ...args) {
  const contract = new Contract(CONTRACT);
  const seq = (await server.getAccount(source.publicKey())).sequenceNumber();
  const call = contract.call(method, ...args);
  // Cuenta nueva en cada build: TransactionBuilder incrementa la secuencia.
  const build = auth => new TransactionBuilder(new Account(source.publicKey(), seq), { fee: "1000000", networkPassphrase: passphrase })
    .addOperation(auth
      ? Operation.invokeHostFunction({ func: call.body().invokeHostFunctionOp().hostFunction(), auth })
      : call)
    .setTimeout(60).build();

  let tx = build();
  let sim = await server.simulateTransaction(tx);
  if (rpc.Api.isSimulationError(sim)) throw new Error(`${method}: ${sim.error}`);

  // Firmas de las otras personas (auth entries que no son de la cuenta origen).
  const entries = sim.result?.auth ?? [];
  if (entries.some(e => e.credentials().switch().name === "sorobanCredentialsAddress")) {
    const until = sim.latestLedger + 100;
    const signed = await Promise.all(entries.map(e => {
      if (e.credentials().switch().name !== "sorobanCredentialsAddress") return e;
      const who = Address.fromScAddress(e.credentials().address().address()).toString();
      return authorizeEntry(e, byAddress[who], until, passphrase);
    }));
    tx = build(signed);
    sim = await server.simulateTransaction(tx);
    if (rpc.Api.isSimulationError(sim)) throw new Error(`${method}: ${sim.error}`);
  }

  const ready = rpc.assembleTransaction(tx, sim).build();
  ready.sign(source);
  const sent = await server.sendTransaction(ready);
  if (sent.status === "ERROR") throw new Error(`${method}: ${sent.errorResult?.result().switch().name}`);
  let res;
  do {
    await new Promise(r => setTimeout(r, 1500));
    res = await server.getTransaction(sent.hash);
  } while (res.status === "NOT_FOUND");
  if (res.status !== "SUCCESS") throw new Error(`${method}: ${res.status}`);
  console.log(`✓ ${method}  https://stellar.expert/explorer/testnet/tx/${sent.hash}`);
}

const member = (k, role) => struct({ account: addr(k), role: str(role), since: u64(0) });
const plan = (name, panels, priceXlm, kwh) =>
  struct({ name: str(name), panels: u32(panels), price: i128(BigInt(priceXlm) * XLM), kwh: i128(kwh) });

const steps = [
  () => invoke(K.marta, "initialize",
    xdr.ScVal.scvVec([
      member(K.marta, "Presidenta ADI"),
      member(K.luis, "Fontanero ASADA"),
      member(K.sofia, "Maestra de la escuela"),
    ]),
    u32(2), new Address(XLM_SAC).toScVal()),
  () => invoke(K.solartico, "create_project",
    addr(K.solartico), addr(K.marta), str("Santa Teresa Solar"),
    xdr.ScVal.scvVec([
      plan("Basico", 1, 50, 250),
      plan("Familiar", 2, 100, 500),
      plan("Comercio", 4, 200, 1000),
    ]),
    u32(3000), u32(30), u32(90)),
  () => invoke(K.ana, "subscribe", addr(K.ana), u32(0), u32(1)),
  () => invoke(K.beto, "subscribe", addr(K.beto), u32(0), u32(0)),
  () => invoke(K.marta, "approve_installation", addr(K.marta), u32(0), i128(5000)),
  () => invoke(K.luis, "approve_installation", addr(K.luis), u32(0), i128(5000)),
  () => invoke(K.marta, "submit_reading", addr(K.marta), u32(0), i128(5180)),
  () => invoke(K.luis, "submit_reading", addr(K.luis), u32(0), i128(5180)),
];
for (const [n, step] of steps.entries()) if (n + 1 >= FROM) await step();
console.log("Demo cargada en", CONTRACT);
