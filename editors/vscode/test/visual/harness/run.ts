import '../../../webview/run/main';
import type { EscurelRunDetail } from '../../../webview/run/run-detail';
import { recordedRunView } from '../../component/run-fixtures';

const el = document.querySelector('escurel-run-detail') as EscurelRunDetail;
el.view = recordedRunView;
