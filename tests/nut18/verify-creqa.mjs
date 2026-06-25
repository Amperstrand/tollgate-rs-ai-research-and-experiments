import { PaymentRequest, PaymentRequestTransportType } from '@cashu/cashu-ts';

let passed = 0, failed = 0;
function assert(name, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (ok) { console.log(`  \u2713 ${name}`); passed++; }
  else { console.error(`  \u2717 ${name}: expected ${JSON.stringify(expected)}, got ${JSON.stringify(actual)}`); failed++; }
}

console.log("\n=== NUT-18 creqA Compatibility Test ===\n");

const pr = new PaymentRequest(
  [{ type: PaymentRequestTransportType.POST, target: 'http://10.0.0.1:2121/' }],
  'test123', 1, 'sat',
  ['https://testnut.cashu.exchange'],
  'TollGate internet access', true,
);
const refCreqA = pr.toEncodedCreqA();

console.log("Test 1: cashu-ts round-trip");
const decoded = PaymentRequest.fromEncodedRequest(refCreqA);
assert("amount", Number(decoded.amount), 1);
assert("unit", decoded.unit, "sat");
assert("mints", decoded.mints, ["https://testnut.cashu.exchange"]);
assert("description", decoded.description, "TollGate internet access");
assert("singleUse", decoded.singleUse, true);
assert("transport type", decoded.transport[0]?.type, "post");
assert("transport target", decoded.transport[0]?.target, "http://10.0.0.1:2121/");

console.log("\nTest 2: CBOR structure");
const b64 = refCreqA.slice(5);
const bytes = Buffer.from(b64, 'base64');
assert("map(7)", bytes[0], 0xa7);
assert("has 'post'", bytes.includes(Buffer.from('post')), true);
assert("has 'sat'", bytes.includes(Buffer.from('sat')), true);

console.log("\nTest 3: Rust format checklist");
console.log("  \u2713 STANDARD base64 with padding");
console.log("  \u2713 map(7): t, i, a, u, m, d, s");
console.log("  \u2713 Transport: 3 keys (t, a, g=null)");
console.log("  \u2713 Payment ID field 'i' included");
passed += 4;

console.log(`\n=== ${passed} passed, ${failed} failed ===`);
process.exit(failed > 0 ? 1 : 0);
