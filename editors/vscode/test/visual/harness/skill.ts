import '../../../webview/skill-page/main';
import type { EscurelSkillPage } from '../../../webview/skill-page/skill-page';
import { orderSkillPage } from '../../component/skill-fixtures';

const el = document.querySelector('escurel-skill-page') as EscurelSkillPage;
el.model = orderSkillPage;
