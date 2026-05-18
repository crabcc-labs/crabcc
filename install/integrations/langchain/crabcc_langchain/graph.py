"""Minimal LangGraph: plan → crabcc lookup → answer."""

from __future__ import annotations

import os
from typing import Annotated, TypedDict

from langchain_core.messages import AIMessage, HumanMessage
from langgraph.graph import END, StateGraph
from langgraph.graph.message import add_messages
from langgraph.prebuilt import ToolNode

from crabcc_langchain.tools import CRABCC_TOOLS


class AgentState(TypedDict):
    messages: Annotated[list, add_messages]


def build_lookup_graph(model):
    """Bind crabcc tools to `model` and return a compiled LangGraph."""
    bound = model.bind_tools(CRABCC_TOOLS)
    tool_node = ToolNode(CRABCC_TOOLS)

    def agent(state: AgentState):
        return {"messages": [bound.invoke(state["messages"])]}

    def should_continue(state: AgentState):
        last = state["messages"][-1]
        if getattr(last, "tool_calls", None):
            return "tools"
        return END

    g = StateGraph(AgentState)
    g.add_node("agent", agent)
    g.add_node("tools", tool_node)
    g.set_entry_point("agent")
    g.add_conditional_edges("agent", should_continue, {"tools": "tools", END: END})
    g.add_edge("tools", "agent")
    return g.compile()


def demo_prompt() -> str:
    return os.environ.get(
        "CRABCC_GRAPH_PROMPT",
        "Where is Store::open defined and who calls it?",
    )


if __name__ == "__main__":
    # Smoke without a live LLM: exercise tools only.
    from crabcc_langchain.tools import crabcc_sym

    print(crabcc_sym.invoke({"name": "main"}))
