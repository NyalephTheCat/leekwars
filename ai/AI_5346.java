import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_5346 extends AI {
public AI_5346() throws LeekRunException {
super(1, 2);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
ops(4); return new SetLeekValue(AI_5346.this, new Object[] { 1l, 2l });
}
protected String getAIString() { return "<snippet 5346>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 5346>", };}

protected int[] getErrorFilesID() { return new int[] {5346, };}

}
