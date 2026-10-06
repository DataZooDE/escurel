import '../../../webview/overview/main';
import type { EscurelOverview } from '../../../webview/overview/overview';
import { morning, quiet } from '../../component/overview-fixtures';

const el = document.querySelector('escurel-overview') as EscurelOverview;
// ?state=quiet draws the empty board; the default is a busy morning.
el.view = new URLSearchParams(location.search).get('state') === 'quiet' ? quiet : morning;
