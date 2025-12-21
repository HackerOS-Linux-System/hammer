package main

import (
	"fmt"
	"os"
	"os/exec"
	"strings"

	"github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/bubbles/list"
	"github.com/charmbracelet/bubbles/textinput"
	"github.com/charmbracelet/bubbles/viewport"
	"github.com/charmbracelet/lipgloss"
)

type state int

const (
	menuState state = iota
	promptPackage
	promptAtomic
	running
	outputState
)

type item struct {
	title      string
	desc       string
	command    string
	hasPackage bool
	hasAtomic  bool
}

func (i item) Title() string       { return i.title }
func (i item) Description() string { return i.desc }
func (i item) FilterValue() string { return i.title }

type model struct {
	state       state
	list        list.Model
	textinput   textinput.Model
	atomic      bool
	packageName string
	currentItem item
	viewport    viewport.Model
	output      string
	err         error
	width       int
	height      int
}

func initialModel() model {
	ti := textinput.New()
	ti.CharLimit = 156
	ti.Width = 30

	items := []list.Item{
		item{title: "Install package", desc: "Install a package (atomic optional)", command: "install", hasPackage: true, hasAtomic: true},
		item{title: "Remove package", desc: "Remove a package (atomic optional)", command: "remove", hasPackage: true, hasAtomic: true},
		item{title: "Update", desc: "Update the system atomically", command: "update", hasPackage: false, hasAtomic: false},
		item{title: "Clean", desc: "Clean up unused resources", command: "clean", hasPackage: false, hasAtomic: false},
		item{title: "Refresh", desc: "Refresh repositories", command: "refresh", hasPackage: false, hasAtomic: false},
		item{title: "Switch", desc: "Switch to a deployment (rollback if no arg)", command: "switch", hasPackage: false, hasAtomic: false},
		item{title: "Deploy", desc: "Create a new deployment", command: "deploy", hasPackage: false, hasAtomic: false},
		item{title: "Status", desc: "Show status", command: "status", hasPackage: false, hasAtomic: false},
		item{title: "History", desc: "Show history", command: "history", hasPackage: false, hasAtomic: false},
		item{title: "Rollback", desc: "Rollback n steps", command: "rollback", hasPackage: false, hasAtomic: false},
		item{title: "Build init", desc: "Initialize build project", command: "build init", hasPackage: false, hasAtomic: false},
		item{title: "Build", desc: "Build atomic ISO", command: "build", hasPackage: false, hasAtomic: false},
		item{title: "About", desc: "Show tool information", command: "about", hasPackage: false, hasAtomic: false},
		item{title: "Quit", desc: "Exit the TUI", command: "quit", hasPackage: false, hasAtomic: false},
	}

	delegate := list.NewDefaultDelegate()
	delegate.Styles.SelectedTitle.Foreground(lipgloss.Color("#00FF00"))

	l := list.New(items, delegate, 0, 0)
	l.Title = "Hammer TUI"
	l.Styles.Title = lipgloss.NewStyle().Bold(true).Foreground(lipgloss.Color("#FAFAFA")).Background(lipgloss.Color("#7D56F4")).Padding(0, 1)

	vp := viewport.New(0, 0)
	vp.Style = lipgloss.NewStyle().BorderStyle(lipgloss.NormalBorder()).BorderForeground(lipgloss.Color("240"))

	return model{
		list:      l,
		textinput: ti,
		viewport:  vp,
	}
}

func (m model) Init() tea.Cmd {
	return nil
}

func (m model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	var cmd tea.Cmd

	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width = msg.Width
		m.height = msg.Height
		m.list.SetSize(msg.Width-4, msg.Height-6)
		m.viewport.Width = msg.Width - 4
		m.viewport.Height = msg.Height - 6
		m.textinput.Width = msg.Width - 4
		return m, nil
	}

	switch m.state {
	case menuState:
		switch msg := msg.(type) {
		case tea.KeyMsg:
			if msg.String() == "enter" {
				i, ok := m.list.SelectedItem().(item)
				if ok {
					m.currentItem = i
					if i.command == "quit" {
						return m, tea.Quit
					}
					if i.hasPackage {
						m.state = promptPackage
						m.textinput.Placeholder = "Enter package name"
						m.textinput.Focus()
						return m, textinput.Blink
					}
					if i.hasAtomic {
						m.state = promptAtomic
						m.textinput.Placeholder = "Atomic? (y/n)"
						m.textinput.Focus()
						return m, textinput.Blink
					}
					m.state = running
					return m, m.runCommand()
				}
			}
		}
		m.list, cmd = m.list.Update(msg)
		return m, cmd
	case promptPackage:
		switch msg := msg.(type) {
		case tea.KeyMsg:
			if msg.String() == "enter" && m.textinput.Value() != "" {
				m.packageName = m.textinput.Value()
				m.textinput.Reset()
				if m.currentItem.hasAtomic {
					m.state = promptAtomic
					m.textinput.Placeholder = "Atomic? (y/n)"
					return m, textinput.Blink
				}
				m.state = running
				return m, m.runCommand()
			} else if msg.String() == "esc" {
				m.state = menuState
				m.textinput.Blur()
				return m, nil
			}
		}
		m.textinput, cmd = m.textinput.Update(msg)
		return m, cmd
	case promptAtomic:
		switch msg := msg.(type) {
		case tea.KeyMsg:
			if msg.String() == "enter" {
				val := strings.ToLower(m.textinput.Value())
				m.atomic = val == "y" || val == "yes"
				m.textinput.Reset()
				m.state = running
				return m, m.runCommand()
			} else if msg.String() == "esc" {
				m.state = menuState
				m.textinput.Blur()
				return m, nil
			}
		}
		m.textinput, cmd = m.textinput.Update(msg)
		return m, cmd
	case running:
		switch msg := msg.(type) {
		case outputMsg:
			m.output = msg.output
			m.err = msg.err
			m.state = outputState
			if m.err != nil {
				m.viewport.SetContent(fmt.Sprintf("Error: %v\n%s", m.err, m.output))
			} else {
				m.viewport.SetContent(m.output)
			}
			return m, nil
		}
		return m, nil
	case outputState:
		switch msg := msg.(type) {
		case tea.KeyMsg:
			if msg.String() == "enter" || msg.String() == "q" || msg.String() == "esc" {
				m.state = menuState
				return m, nil
			}
		}
		m.viewport, cmd = m.viewport.Update(msg)
		return m, cmd
	}

	return m, nil
}

type outputMsg struct {
	output string
	err    error
}

func (m model) runCommand() tea.Cmd {
	return func() tea.Msg {
		args := strings.Split(m.currentItem.command, " ")
		if m.currentItem.hasAtomic && m.atomic {
			args = append(args, "--atomic")
		}
		if m.currentItem.hasPackage {
			args = append(args, m.packageName)
		}
		c := exec.Command("hammer", args...)
		output, err := c.CombinedOutput()
		return outputMsg{output: string(output), err: err}
	}
}

func (m model) View() string {
	baseStyle := lipgloss.NewStyle().Padding(1, 2)

	switch m.state {
	case menuState:
		return baseStyle.Render(m.list.View())
	case promptPackage, promptAtomic:
		return baseStyle.Render(m.textinput.View())
	case running:
		return baseStyle.Render("Running command...")
	case outputState:
		return baseStyle.Render(m.viewport.View() + "\nPress enter or q to return")
	}
	return ""
}

func main() {
	p := tea.NewProgram(initialModel(), tea.WithAltScreen())
	if _, err := p.Run(); err != nil {
		fmt.Println("Error running program:", err)
		os.Exit(1)
	}
}
