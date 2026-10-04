// Creates the demo's `orders-db`: a real SQLite file the gateway attaches (read for the rows, read-write
// for a reviewed write-back). Run by demo/run.sh before the gateway starts; idempotent.
import { mkdirSync, rmSync } from 'node:fs';
import { dirname } from 'node:path';
import { DatabaseSync } from 'node:sqlite';

const path = process.argv[2];
mkdirSync(dirname(path), { recursive: true });
rmSync(path, { force: true });
const db = new DatabaseSync(path);
db.exec(`CREATE TABLE orders (
  order_no TEXT PRIMARY KEY,
  customer TEXT NOT NULL,
  material TEXT NOT NULL,
  qty      INTEGER NOT NULL,
  net_value REAL NOT NULL,
  status   TEXT NOT NULL
)`);
const insert = db.prepare('INSERT INTO orders VALUES (?, ?, ?, ?, ?, ?)');
const rows = [
  ['SO-100231', 'Meier Gussteile', 'Flanschrohr DN100', 40, 18400.0, 'open'],
  ['SO-100232', 'Meier Gussteile', 'Dichtung EPDM 120', 500, 2150.5, 'open'],
  ['SO-100245', 'Nordform GmbH', 'Stahlblech 2mm', 120, 9600.0, 'open'],
  ['SO-100251', 'Nordform GmbH', 'Schraube M12', 2000, 780.0, 'shipped'],
  ['SO-100263', 'Iberica Forja', 'Schmiedeteil K7', 15, 12750.0, 'open'],
];
for (const r of rows) insert.run(...r);
db.close();
