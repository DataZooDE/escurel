/// ADMIN — manage RBAC group membership. A compact panel that adds /
/// removes subjects from a group and lists the current members of the
/// group entered in the group-id field. Calls the three admin tools
/// (`add_group_member` / `remove_group_member` / `list_group_members`);
/// the backend gates them to admins (JSON-RPC -32001 otherwise), so the
/// UI surfaces the error rather than auth-gating the surface itself.
library;

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../client/models.dart';
import '../state/providers.dart';
import '../theme/app_theme.dart';

class GroupMembersPane extends ConsumerStatefulWidget {
  const GroupMembersPane({super.key});
  @override
  ConsumerState<GroupMembersPane> createState() => _GroupMembersPaneState();
}

class _GroupMembersPaneState extends ConsumerState<GroupMembersPane> {
  final _groupController = TextEditingController();
  final _subjectController = TextEditingController();
  List<GroupMember> _members = const [];
  String _status = '';
  bool _busy = false;

  Future<void> _refresh() async {
    final groupId = _groupController.text.trim();
    if (groupId.isEmpty) {
      setState(() => _members = const []);
      return;
    }
    try {
      final members = await ref
          .read(escurelClientProvider)
          .listGroupMembers(groupId);
      if (mounted) setState(() => _members = members);
    } catch (e) {
      if (mounted) setState(() => _status = 'error: $e');
    }
  }

  Future<void> _add() async {
    final groupId = _groupController.text.trim();
    final subject = _subjectController.text.trim();
    if (groupId.isEmpty || subject.isEmpty || _busy) return;
    setState(() {
      _busy = true;
      _status = 'adding…';
    });
    try {
      await ref.read(escurelClientProvider).addGroupMember(groupId, subject);
      _subjectController.clear();
      await _refresh();
      setState(() => _status = 'added');
    } catch (e) {
      setState(() => _status = 'error: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _remove(String subject) async {
    final groupId = _groupController.text.trim();
    if (groupId.isEmpty || _busy) return;
    setState(() {
      _busy = true;
      _status = 'removing…';
    });
    try {
      await ref.read(escurelClientProvider).removeGroupMember(groupId, subject);
      await _refresh();
      setState(() => _status = 'removed');
    } catch (e) {
      setState(() => _status = 'error: $e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  void dispose() {
    _groupController.dispose();
    _subjectController.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final text = Theme.of(context).textTheme;
    return Semantics(
      label: 'group-members-pane',
      container: true,
      explicitChildNodes: true,
      child: Container(
        decoration: const BoxDecoration(
          color: kSurfaceContainerLowest,
          border: Border(top: BorderSide(color: kOutlineVariant)),
        ),
        padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 10),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          mainAxisSize: MainAxisSize.min,
          children: [
            Row(
              children: [
                const Icon(Icons.group_outlined, size: 18, color: kPrimary),
                const SizedBox(width: 10),
                Text(
                  'GROUP MEMBERS',
                  style: text.labelSmall?.copyWith(
                    color: kOutline,
                    letterSpacing: 1,
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Semantics(
                    label: 'group-members-group-field',
                    textField: true,
                    container: true,
                    explicitChildNodes: true,
                    child: TextField(
                      controller: _groupController,
                      onSubmitted: (_) => _refresh(),
                      decoration: const InputDecoration(
                        isDense: true,
                        border: InputBorder.none,
                        hintText: 'group id →',
                      ),
                    ),
                  ),
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Semantics(
                    label: 'group-member-subject-field',
                    textField: true,
                    container: true,
                    explicitChildNodes: true,
                    child: TextField(
                      controller: _subjectController,
                      onSubmitted: (_) => _add(),
                      decoration: const InputDecoration(
                        isDense: true,
                        border: InputBorder.none,
                        hintText: 'subject →',
                      ),
                    ),
                  ),
                ),
                const SizedBox(width: 12),
                Semantics(
                  label: 'group-member-add',
                  button: true,
                  onTap: _add,
                  excludeSemantics: true,
                  child: FilledButton.icon(
                    onPressed: _busy ? null : _add,
                    icon: const Icon(Icons.person_add_alt_1, size: 16),
                    label: const Text('add'),
                  ),
                ),
                if (_status.isNotEmpty) ...[
                  const SizedBox(width: 12),
                  Semantics(
                    label: 'group-members-status',
                    child: Text(
                      _status,
                      style: text.labelSmall?.copyWith(color: kOutline),
                    ),
                  ),
                ],
              ],
            ),
            if (_members.isNotEmpty) ...[
              const SizedBox(height: 8),
              Semantics(
                label: 'group-members-list',
                container: true,
                explicitChildNodes: true,
                child: Wrap(
                  spacing: 8,
                  runSpacing: 4,
                  children: [
                    for (final m in _members)
                      Chip(
                        label: Text(m.subject, style: text.labelSmall),
                        onDeleted: _busy ? null : () => _remove(m.subject),
                        deleteIcon: const Icon(Icons.close, size: 14),
                        deleteButtonTooltipMessage: 'remove',
                      ),
                  ],
                ),
              ),
            ],
          ],
        ),
      ),
    );
  }
}
