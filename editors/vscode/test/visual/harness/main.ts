// The visual harness: the built component with the realistic fixture.
import '../../../webview/page-as-ui/main';
import type { EscurelPageAsUi } from '../../../webview/page-as-ui/page-as-ui';
import { orderPage } from '../../component/fixtures';

const el = document.querySelector('escurel-page-as-ui') as EscurelPageAsUi;
el.model = orderPage;
