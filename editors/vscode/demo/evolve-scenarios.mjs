// Demo only. NOT part of the shipped extension.
//
// Plays the Evolve scenarios into a running demo: two real searches (no model spend) started
// through the Evolve service as the demo user, then one owner-created comparison page for each,
// ready for a click on "Compute comparison".
//
//   bin-packing  a scripted search finds a better packing; compared against the seed.
//   assortment   a scripted search proposes the classical top-N shelf (which delists the whole
//                Household category) and then a substitution-aware assortment; compared against
//                the top-N program, the winner's parent.
//
// Everything is synthetic and the proposals are scripted (not model judgment). A comparison is a
// search-time replay on the training instance: it explains what changed and is not validation.
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));

export const SCENARIOS = [
  {
    id: 'bin-packing',
    experiment: 'demo-bin-packing',
    spec: 'p0-bin-packing.json',
    comparison: 'demo-bin-packing-vs-seed',
    baseline: 'seed',
  },
  {
    id: 'assortment',
    experiment: 'demo-assortment',
    spec: 'p5-assortment.json',
    comparison: 'demo-assortment-vs-top-n',
    baseline: 'parent',
  },
];

async function waitForCompletion(evolveCall, experiment) {
  for (let i = 0; i < 600; i += 1) {
    const body = await evolveCall('evolve_status', { experiment });
    if (body && body.evolve_status === 'completed') return;
    if (body && ['failed', 'cancelled'].includes(body.evolve_status))
      throw new Error(`experiment ${experiment} ended as ${body.evolve_status}`);
    await new Promise((r) => setTimeout(r, 200));
  }
  throw new Error(`experiment ${experiment} did not complete`);
}

export function comparisonPage({ id, owner, experiment, baseline }) {
  return [
    '---',
    'kind: instance',
    'skill: evolve_comparison',
    `id: ${id}`,
    `owner_subject: ${JSON.stringify(owner)}`,
    `experiment: ${experiment}`,
    `baseline: ${baseline}`,
    'candidate: winner',
    'status: requested',
    'next_comparison_action: evolve_compare',
    '---',
    '',
    `# Comparison of ${experiment}`,
    '',
    'Click Compute comparison to ask Evolve what the winner changed relative to the baseline.',
    '',
  ].join('\n');
}

/**
 * Start both searches as `owner`, wait for them, and create the two comparison pages.
 * `evolveCall(tool, args)` calls Evolve and `gatewayCall(tool, args)` calls the Escurel gateway,
 * both as the demo user and returning the parsed result (throwing on an error).
 */
export async function playScenarios({ evolveCall, gatewayCall, owner, scenarios = SCENARIOS }) {
  const played = [];
  for (const s of scenarios) {
    const spec = JSON.parse(readFileSync(join(HERE, 'evolve', s.spec), 'utf8'));
    await evolveCall('evolve_start', { ...spec, experiment: s.experiment });
    await waitForCompletion(evolveCall, s.experiment);
    const page = `markdown/instances/evolve_comparison/${s.comparison}.md`;
    const written = await gatewayCall('update_page', {
      page_id: page,
      content: comparisonPage({
        id: s.comparison,
        owner,
        experiment: s.experiment,
        baseline: s.baseline,
      }),
      base_sha256: '',
    });
    if (!written.ok)
      throw new Error(`comparison page ${s.comparison}: ${JSON.stringify(written.issues)}`);
    played.push({ scenario: s.id, experiment: s.experiment, comparison: s.comparison, page });
  }
  return played;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const [gatewayFile, bearerFile, evolveUrl] = process.argv.slice(2);
  const gw = JSON.parse(readFileSync(gatewayFile, 'utf8').split('\n')[0]);
  const bearer = JSON.parse(readFileSync(bearerFile, 'utf8')).bearer;
  const owner = process.env.ESCUREL_DEMO_SUBJECT || 'alice';
  const post = async (url, headers, body) => {
    const res = await fetch(url, {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
        authorization: `Bearer ${bearer}`,
        ...headers,
      },
      body: JSON.stringify(body),
    });
    const text = await res.text();
    if (!res.ok) throw new Error(`${url}: HTTP ${res.status}: ${text}`);
    return JSON.parse(text);
  };
  const evolveCall = (tool, args) => post(`${evolveUrl}/`, { 'X-Triton-Tool': tool }, args);
  const gatewayCall = async (name, args) => {
    const body = await post(
      `${gw.gateway_url}/mcp`,
      {},
      {
        jsonrpc: '2.0',
        id: 1,
        method: 'tools/call',
        params: { name, arguments: args },
      },
    );
    if (body.error) throw new Error(`${name}: ${JSON.stringify(body.error)}`);
    return body.result.structuredContent;
  };
  const played = await playScenarios({ evolveCall, gatewayCall, owner });
  process.stdout.write(JSON.stringify({ evolve: played }) + '\n');
}
