import React, {ReactNode, useState} from "react";
import {Button, Dialog, DialogBody, DialogFooter, DialogHeader} from "@material-tailwind/react";
import {projectCreateBackup, TauriProject} from "@/lib/bindings";
import {toastNormal, toastSuccess, toastThrownError} from "@/lib/toast";
import {tc, tt} from "@/lib/i18n";
import {nop} from "@/lib/nop";
import {callAsyncCommand} from "@/lib/call-async-command";

// string if remove project by path
type Project = TauriProject | {
	path: string,
	name: string,
};

type State = {
	type: 'idle',
} | {
	type: 'backing-up',
	cancel: () => void,
}

type Params = {}

type Result = {
	startBackup: (project: Project) => void,
	dialog: ReactNode,
}

export function useBackupProjectModal(_: Params = {}): Result {
	const [state, setState] = useState<State>({type: 'idle'});

	const startBackup = async (project: Project) => {
		try {
			const [cancel, promise] = callAsyncCommand(projectCreateBackup, [project.path], nop);
			setState({type: 'backing-up', cancel});
			const channel = await promise;
			if (channel == 'cancelled') {
				toastNormal(tt("projects:toast:backup canceled"));
			} else {
				toastSuccess(tt("projects:toast:backup succeeded"));
			}
			setState({type: 'idle'});
		} catch (e) {
			console.error("Error creating backup", e);
			setState({type: 'idle'});
			toastThrownError(e);
		}
	};

	let dialog: ReactNode = null;

	switch (state.type) {
		case "idle":
			break;
		case "backing-up":
			dialog = (
				<Dialog open handler={nop} className={'whitespace-normal'}>
					<DialogHeader>{tc("projects:dialog:backup header")}</DialogHeader>
					<DialogBody>
						{tc("projects:dialog:creating backup...")}
					</DialogBody>
					<DialogFooter>
						<Button className="mr-1" onClick={state.cancel}>{tc("general:button:cancel")}</Button>
					</DialogFooter>
				</Dialog>
			);
			break;
		default:
			let _: never = state;
	}

	return {startBackup, dialog}
}
